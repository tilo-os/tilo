//! End-to-end: cooperative cryptographic redaction flowing through the
//! broker. Eight scenarios cover the happy path, role enforcement
//! (`RoleForbidsOpClass`), survival across a `Compaction` checkpoint,
//! scrollback filtering, a late-arriving already-redacted op being
//! dropped (`OpAlreadyRedacted`) rather than re-fanned, redaction of an
//! MLS-encrypted op ("mark redacted, keep ciphertext" — never decrypts
//! during validation), recursive redaction being rejected
//! (`RecursiveRedaction`), and the audit record's fields.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use collab_fixtures::{role_attestation, signed_op, signed_op_encrypted};
use collab_types::{
    ArtifactId, ArtifactKind, AttestationId, ChannelId, ChannelState, CompactionCheckpoint,
    Ed25519PublicKey, EncryptionMetadata, OpClass, OpId, RedactRequest, RedactedMarker, Role,
    SignedOp, ValidationError, encode_compaction_payload, encode_redact_payload, generate_keypair,
    hash_op_prefix, hash_target_op, parse_redact_payload,
};
use ed25519_dalek::SigningKey;
use shell::shell::collab_broker;
use ui::collaboration::{CollabEvent, CollabRequest};

// frame helpers

fn write_frame<T: serde::Serialize>(stream: &mut UnixStream, v: &T) {
    let bytes = serde_json::to_vec(v).expect("serialize");
    let len = (bytes.len() as u32).to_le_bytes();
    stream.write_all(&len).expect("write len");
    stream.write_all(&bytes).expect("write body");
}

fn read_event(stream: &mut UnixStream, timeout: Duration) -> Option<CollabEvent> {
    stream.set_read_timeout(Some(timeout)).expect("set timeout");
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).ok()?;
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn drain_events(stream: &mut UnixStream, timeout: Duration) -> Vec<CollabEvent> {
    let deadline = Instant::now() + timeout;
    let mut out = Vec::new();
    while Instant::now() < deadline {
        let remain = deadline.saturating_duration_since(Instant::now());
        match read_event(stream, remain) {
            Some(ev) => out.push(ev),
            None => break,
        }
    }
    out
}

fn unique_socket(label: &str) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!("qos-broker-rd-{label}-{pid}-{nanos}.sock"))
}

/// The collaboration cluster every client in these scenarios joins. The
/// broker routes CRDT traffic by the `(app_id, cluster_id)` pair a
/// client announces on `Hello`, so peers meant to observe each other
/// must announce the same pair — a per-client random uuid would put
/// every client in a cluster of its own.
fn test_cluster() -> uuid::Uuid {
    uuid::Uuid::from_u128(0x3f2b_1c00_0000_4000_8000_0000_0000_0001)
}

/// Block until `n` clients have announced their route. Connecting and
/// saying `Hello` are separate steps; only the second makes a client
/// addressable, so a send that races another client's `Hello` would
/// reach nobody.
fn wait_for_announced(handle: &collab_broker::BrokerHandle, n: usize) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if handle.announced_peer_count() >= n {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!(
        "only {} of {n} clients announced a route within 2s",
        handle.announced_peer_count()
    );
}

fn wait_for_broker(socket: &PathBuf) -> UnixStream {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match UnixStream::connect(socket) {
            Ok(s) => return s,
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("could not connect to broker: {e}"),
        }
    }
}

// attestation / op factories

/// A channel state with `owner_pk` as the authorised signer, plus an
/// Owner attestation for `owner_pk` and an Editor attestation for
/// `editor_pk`.
fn channel_state_with(
    owner_sk: &SigningKey,
    owner_pk: Ed25519PublicKey,
    editor_pk: Ed25519PublicKey,
    channel: ChannelId,
    artifact: ArtifactId,
) -> (ChannelState, AttestationId, AttestationId) {
    let owner_ra = role_attestation(owner_sk, owner_pk, owner_pk, channel, Role::Owner);
    let owner_att_id = owner_ra.attestation.id;
    let editor_ra = role_attestation(owner_sk, owner_pk, editor_pk, channel, Role::Editor);
    let editor_att_id = editor_ra.attestation.id;

    let mut state = ChannelState::default();
    state.authorised_signers.push(owner_pk);
    state.artifacts.insert(artifact, ArtifactKind::Note);
    state.register_attestation(owner_ra);
    state.register_attestation(editor_ra);
    (state, owner_att_id, editor_att_id)
}

/// Signed Redact envelope targeting `target_op`, referencing `owner_att`.
fn build_redact_op(
    owner_sk: &SigningKey,
    owner_pk: Ed25519PublicKey,
    artifact: ArtifactId,
    owner_att: AttestationId,
    target_op: &SignedOp,
    reason: &str,
    seq: u64,
) -> SignedOp {
    let target_op_id = OpId::from_op(target_op);
    let target_op_hash = hash_target_op(target_op).expect("hash target");
    let req = RedactRequest {
        artifact,
        target_op_id,
        target_op_hash,
        reason: reason.into(),
        redacted_by: owner_pk,
        redacted_at: SystemTime::now(),
    };
    let payload = encode_redact_payload(&req).expect("encode redact");
    signed_op(
        owner_sk,
        owner_pk,
        artifact,
        owner_att,
        OpClass::Redact,
        payload,
        seq,
    )
}

// Scenario 1: happy path

#[test]
fn owner_can_redact_a_recent_op() {
    let socket = unique_socket("happy");
    let handle = collab_broker::spawn(socket.clone()).expect("broker bind");

    let (owner_sk, owner_pk) = generate_keypair();
    let (editor_sk, editor_pk) = generate_keypair();
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();
    let (state, owner_att, editor_att) =
        channel_state_with(&owner_sk, owner_pk, editor_pk, channel, artifact);
    handle.register_channel_state(artifact, state);

    let mut editor = wait_for_broker(&socket);
    let mut owner = wait_for_broker(&socket);
    let mut observer = wait_for_broker(&socket);
    for s in [&mut editor, &mut owner, &mut observer] {
        write_frame(
            s,
            &CollabRequest::Hello {
                app_id: "test".into(),
                cluster_id: test_cluster(),
                record: None,
                handle: None,
            },
        );
    }
    wait_for_announced(&handle, 3);

    let secret_payload = b"PII: alice@example.com home address 123 Main St".to_vec();
    let target_op = signed_op(
        &editor_sk,
        editor_pk,
        artifact,
        editor_att,
        OpClass::Splice,
        secret_payload.clone(),
        1,
    );
    write_frame(
        &mut editor,
        &CollabRequest::SignedSyncIncremental {
            envelope: Box::new(target_op.clone()),
        },
    );
    let _ = drain_events(&mut observer, Duration::from_millis(300));
    let _ = drain_events(&mut owner, Duration::from_millis(150));

    let redact = build_redact_op(
        &owner_sk,
        owner_pk,
        artifact,
        owner_att,
        &target_op,
        "GDPR request from data subject",
        10,
    );
    write_frame(
        &mut owner,
        &CollabRequest::SignedSyncIncremental {
            envelope: Box::new(redact),
        },
    );
    let fanned = drain_events(&mut observer, Duration::from_millis(400));
    assert!(
        fanned.iter().any(|e| matches!(
            e,
            CollabEvent::SignedCRDTIncremental { envelope, .. } if envelope.op_class == OpClass::Redact
        )),
        "observer must see the Redact event fan out",
    );

    let records = handle.redactions_for(artifact);
    assert_eq!(records.len(), 1, "exactly one redaction recorded");
    let rec = &records[0];
    assert_eq!(rec.target_op_id, OpId::from_op(&target_op));
    assert_eq!(rec.redactor_pk, owner_pk);
    assert_eq!(rec.reason, "GDPR request from data subject");

    write_frame(
        &mut observer,
        &CollabRequest::FetchScrollback {
            artifact,
            before_seq: 5,
            max: 10,
        },
    );
    let event = read_event(&mut observer, Duration::from_millis(500)).expect("ScrollbackBatch");
    match event {
        CollabEvent::ScrollbackBatch { ops, .. } => {
            for op in ops {
                if op.seq == 1 {
                    assert_ne!(
                        op.payload, secret_payload,
                        "redacted op's payload must not be the original secret",
                    );
                    let marker_bytes = RedactedMarker::v1().canonical().unwrap();
                    assert_eq!(op.payload, marker_bytes);
                }
            }
        }
        other => panic!("expected ScrollbackBatch, got {other:?}"),
    }

    drop(editor);
    drop(owner);
    drop(observer);
    drop(handle);
    let _ = std::fs::remove_file(&socket);
}

// Scenario 2: editor cannot redact

#[test]
fn editor_redact_request_is_rejected() {
    let socket = unique_socket("editor-redact");
    let handle = collab_broker::spawn(socket.clone()).expect("broker bind");

    let (owner_sk, owner_pk) = generate_keypair();
    let (editor_sk, editor_pk) = generate_keypair();
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();
    let (state, _, editor_att) =
        channel_state_with(&owner_sk, owner_pk, editor_pk, channel, artifact);
    handle.register_channel_state(artifact, state);

    let mut editor = wait_for_broker(&socket);
    let mut observer = wait_for_broker(&socket);
    for s in [&mut editor, &mut observer] {
        write_frame(
            s,
            &CollabRequest::Hello {
                app_id: "test".into(),
                cluster_id: test_cluster(),
                record: None,
                handle: None,
            },
        );
    }
    wait_for_announced(&handle, 2);

    // Never delivered to the broker — just needed to build a well-formed
    // Redact (target_op_id / target_op_hash).
    let target = signed_op(
        &editor_sk,
        editor_pk,
        artifact,
        editor_att,
        OpClass::Splice,
        b"victim".to_vec(),
        1,
    );

    let req = RedactRequest {
        artifact,
        target_op_id: OpId::from_op(&target),
        target_op_hash: hash_target_op(&target).unwrap(),
        reason: "feeling like it".into(),
        redacted_by: editor_pk,
        redacted_at: SystemTime::now(),
    };
    let payload = encode_redact_payload(&req).unwrap();
    let bad_redact = signed_op(
        &editor_sk,
        editor_pk,
        artifact,
        editor_att,
        OpClass::Redact,
        payload,
        7,
    );
    write_frame(
        &mut editor,
        &CollabRequest::SignedSyncIncremental {
            envelope: Box::new(bad_redact),
        },
    );

    let stray = read_event(&mut observer, Duration::from_millis(250));
    assert!(
        stray.is_none(),
        "Editor-issued Redact must not be fanned out"
    );

    let audit = handle.audit_for(artifact);
    assert!(
        audit
            .iter()
            .any(|e| matches!(e.rejection, Some(ValidationError::RoleForbidsOpClass))),
        "audit must record RoleForbidsOpClass, got {:?}",
        audit.iter().map(|e| &e.rejection).collect::<Vec<_>>(),
    );
    assert!(handle.redactions_for(artifact).is_empty());

    drop(editor);
    drop(observer);
    drop(handle);
    let _ = std::fs::remove_file(&socket);
}

// Scenario 3: redaction persists across compaction

#[test]
fn redaction_persists_through_audit_log_across_compaction() {
    let socket = unique_socket("redact-then-compact");
    let handle = collab_broker::spawn(socket.clone()).expect("broker bind");

    let (owner_sk, owner_pk) = generate_keypair();
    let (editor_sk, editor_pk) = generate_keypair();
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();
    let (state, owner_att, editor_att) =
        channel_state_with(&owner_sk, owner_pk, editor_pk, channel, artifact);
    handle.register_channel_state(artifact, state);

    let mut editor = wait_for_broker(&socket);
    let mut owner = wait_for_broker(&socket);
    for s in [&mut editor, &mut owner] {
        write_frame(
            s,
            &CollabRequest::Hello {
                app_id: "test".into(),
                cluster_id: test_cluster(),
                record: None,
                handle: None,
            },
        );
    }
    wait_for_announced(&handle, 2);

    let mut ops: Vec<SignedOp> = Vec::new();
    for seq in 1..=3_u64 {
        let op = signed_op(
            &editor_sk,
            editor_pk,
            artifact,
            editor_att,
            OpClass::Splice,
            format!("op-{seq}").into_bytes(),
            seq,
        );
        ops.push(op.clone());
        write_frame(
            &mut editor,
            &CollabRequest::SignedSyncIncremental {
                envelope: Box::new(op),
            },
        );
    }
    let _ = drain_events(&mut owner, Duration::from_millis(300));

    let redact = build_redact_op(
        &owner_sk, owner_pk, artifact, owner_att, &ops[1], "policy", 10,
    );
    write_frame(
        &mut owner,
        &CollabRequest::SignedSyncIncremental {
            envelope: Box::new(redact),
        },
    );
    let _ = drain_events(&mut editor, Duration::from_millis(300));
    assert_eq!(
        handle.redactions_for(artifact).len(),
        1,
        "redaction record present before compaction"
    );

    // Compaction's prefix hash must be computed over the **original**
    // envelope bytes, not the post-redaction marker bytes.
    let prefix = ops
        .iter()
        .filter(|o| o.seq <= 3)
        .cloned()
        .collect::<Vec<_>>();
    let cp = CompactionCheckpoint {
        artifact,
        up_to_seq: 3,
        prefix_hash: hash_op_prefix(&prefix).unwrap(),
    };
    let cp_bytes = encode_compaction_payload(&cp).unwrap();
    let comp_op = signed_op(
        &owner_sk,
        owner_pk,
        artifact,
        owner_att,
        OpClass::Compaction,
        cp_bytes,
        20,
    );
    write_frame(
        &mut owner,
        &CollabRequest::SignedSyncIncremental {
            envelope: Box::new(comp_op),
        },
    );
    let _ = drain_events(&mut editor, Duration::from_millis(300));

    let records = handle.redactions_for(artifact);
    assert_eq!(
        records.len(),
        1,
        "redaction record must outlive the compaction; got {records:?}",
    );
    assert_eq!(records[0].target_op_id, OpId::from_op(&ops[1]));

    drop(editor);
    drop(owner);
    drop(handle);
    let _ = std::fs::remove_file(&socket);
}

// Scenario 4: redacted op is filtered from scrollback

#[test]
fn redacted_op_is_filtered_from_scrollback() {
    let socket = unique_socket("filter-scrollback");
    let handle = collab_broker::spawn(socket.clone()).expect("broker bind");

    let (owner_sk, owner_pk) = generate_keypair();
    let (editor_sk, editor_pk) = generate_keypair();
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();
    let (state, owner_att, editor_att) =
        channel_state_with(&owner_sk, owner_pk, editor_pk, channel, artifact);
    handle.register_channel_state(artifact, state);

    let mut editor = wait_for_broker(&socket);
    let mut owner = wait_for_broker(&socket);
    let mut observer = wait_for_broker(&socket);
    for s in [&mut editor, &mut owner, &mut observer] {
        write_frame(
            s,
            &CollabRequest::Hello {
                app_id: "test".into(),
                cluster_id: test_cluster(),
                record: None,
                handle: None,
            },
        );
    }
    wait_for_announced(&handle, 3);

    let confidential = b"the password is hunter2".to_vec();
    let target = signed_op(
        &editor_sk,
        editor_pk,
        artifact,
        editor_att,
        OpClass::Splice,
        confidential.clone(),
        5,
    );
    write_frame(
        &mut editor,
        &CollabRequest::SignedSyncIncremental {
            envelope: Box::new(target.clone()),
        },
    );
    let _ = drain_events(&mut observer, Duration::from_millis(300));
    let _ = drain_events(&mut owner, Duration::from_millis(150));

    let redact = build_redact_op(
        &owner_sk,
        owner_pk,
        artifact,
        owner_att,
        &target,
        "leaked password",
        20,
    );
    write_frame(
        &mut owner,
        &CollabRequest::SignedSyncIncremental {
            envelope: Box::new(redact),
        },
    );
    let _ = drain_events(&mut observer, Duration::from_millis(300));

    write_frame(
        &mut observer,
        &CollabRequest::FetchScrollback {
            artifact,
            before_seq: 10,
            max: 10,
        },
    );
    let event = read_event(&mut observer, Duration::from_millis(500)).expect("ScrollbackBatch");
    match event {
        CollabEvent::ScrollbackBatch { ops, .. } => {
            let target_seq_op = ops
                .iter()
                .find(|o| o.seq == 5)
                .expect("the seq-5 envelope is still in the log (marker payload)");
            assert_ne!(
                target_seq_op.payload, confidential,
                "the original confidential payload must not be in scrollback",
            );
            assert_eq!(
                target_seq_op.payload,
                RedactedMarker::v1().canonical().unwrap(),
                "the seq-5 entry must carry the canonical redacted marker",
            );
        }
        other => panic!("expected ScrollbackBatch, got {other:?}"),
    }

    drop(editor);
    drop(owner);
    drop(observer);
    drop(handle);
    let _ = std::fs::remove_file(&socket);
}

// Scenario 5: late-arriving already-redacted op is dropped

#[test]
fn late_arriving_op_with_already_redacted_id_is_dropped() {
    let socket = unique_socket("late-arrival");
    let handle = collab_broker::spawn(socket.clone()).expect("broker bind");

    let (owner_sk, owner_pk) = generate_keypair();
    let (editor_sk, editor_pk) = generate_keypair();
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();
    let (state, owner_att, editor_att) =
        channel_state_with(&owner_sk, owner_pk, editor_pk, channel, artifact);
    handle.register_channel_state(artifact, state);

    let mut owner = wait_for_broker(&socket);
    let mut buggy_peer = wait_for_broker(&socket);
    let mut observer = wait_for_broker(&socket);
    for s in [&mut owner, &mut buggy_peer, &mut observer] {
        write_frame(
            s,
            &CollabRequest::Hello {
                app_id: "test".into(),
                cluster_id: test_cluster(),
                record: None,
                handle: None,
            },
        );
    }
    wait_for_announced(&handle, 3);

    // Not pushed through the broker yet — redacted first, to simulate a
    // late arrival from a peer who never got the Redact.
    let target = signed_op(
        &editor_sk,
        editor_pk,
        artifact,
        editor_att,
        OpClass::Splice,
        b"sensitive content".to_vec(),
        3,
    );

    let redact = build_redact_op(
        &owner_sk,
        owner_pk,
        artifact,
        owner_att,
        &target,
        "pre-emptive",
        50,
    );
    write_frame(
        &mut owner,
        &CollabRequest::SignedSyncIncremental {
            envelope: Box::new(redact),
        },
    );
    let _ = drain_events(&mut observer, Duration::from_millis(300));
    assert_eq!(handle.redactions_for(artifact).len(), 1);

    // The "buggy peer" re-broadcasts the original (pre-redaction) envelope,
    // e.g. because it queued it while offline.
    write_frame(
        &mut buggy_peer,
        &CollabRequest::SignedSyncIncremental {
            envelope: Box::new(target),
        },
    );

    let events = drain_events(&mut observer, Duration::from_millis(400));
    for event in &events {
        if let CollabEvent::SignedCRDTIncremental { envelope, .. } = event {
            assert_ne!(
                envelope.payload,
                b"sensitive content".as_slice(),
                "the late-arriving original payload must not be fanned out",
            );
        }
    }

    let audit = handle.audit_for(artifact);
    assert!(
        audit
            .iter()
            .any(|e| matches!(e.rejection, Some(ValidationError::OpAlreadyRedacted { .. }))),
        "audit must record OpAlreadyRedacted, got {:?}",
        audit.iter().map(|e| &e.rejection).collect::<Vec<_>>(),
    );

    drop(owner);
    drop(buggy_peer);
    drop(observer);
    drop(handle);
    let _ = std::fs::remove_file(&socket);
}

// Scenario 6: redaction of MLS-encrypted op succeeds

#[test]
fn redaction_of_mls_encrypted_op_succeeds() {
    // A Redact targeting an MLS-encrypted op never decrypts it — validator
    // role/redaction checks apply regardless of encryption. The op-log
    // payload is rewritten to the marker; the original ciphertext stays in
    // `original_envelopes` so audit can re-verify `target_op_hash`.
    let socket = unique_socket("redact-mls");
    let handle = collab_broker::spawn(socket.clone()).expect("broker bind");

    let (owner_sk, owner_pk) = generate_keypair();
    let (editor_sk, editor_pk) = generate_keypair();
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();
    let (state, owner_att, editor_att) =
        channel_state_with(&owner_sk, owner_pk, editor_pk, channel, artifact);
    handle.register_channel_state(artifact, state);

    let mut editor = wait_for_broker(&socket);
    let mut owner = wait_for_broker(&socket);
    for s in [&mut editor, &mut owner] {
        write_frame(
            s,
            &CollabRequest::Hello {
                app_id: "test".into(),
                cluster_id: test_cluster(),
                record: None,
                handle: None,
            },
        );
    }
    wait_for_announced(&handle, 2);

    // Arbitrary bytes standing in for ciphertext — the broker treats it
    // opaquely.
    let ciphertext = b"\x00\x01\xfe\xff...mls-ciphertext-blob".to_vec();
    let target = signed_op_encrypted(
        &editor_sk,
        editor_pk,
        artifact,
        editor_att,
        OpClass::Splice,
        ciphertext,
        2,
        EncryptionMetadata { epoch: 7, channel },
    );
    write_frame(
        &mut editor,
        &CollabRequest::SignedSyncIncremental {
            envelope: Box::new(target.clone()),
        },
    );
    let _ = drain_events(&mut owner, Duration::from_millis(200));

    let redact = build_redact_op(
        &owner_sk,
        owner_pk,
        artifact,
        owner_att,
        &target,
        "compliance — encrypted PII",
        30,
    );
    write_frame(
        &mut owner,
        &CollabRequest::SignedSyncIncremental {
            envelope: Box::new(redact),
        },
    );
    let _ = drain_events(&mut editor, Duration::from_millis(300));

    let records = handle.redactions_for(artifact);
    assert_eq!(records.len(), 1, "MLS-encrypted target was still redacted");
    assert_eq!(records[0].target_op_id, OpId::from_op(&target));

    drop(editor);
    drop(owner);
    drop(handle);
    let _ = std::fs::remove_file(&socket);
}

// Scenario 7: recursive redaction is rejected

#[test]
fn recursive_redaction_is_rejected() {
    let socket = unique_socket("recursive");
    let handle = collab_broker::spawn(socket.clone()).expect("broker bind");

    let (owner_sk, owner_pk) = generate_keypair();
    let (editor_sk, editor_pk) = generate_keypair();
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();
    let (state, owner_att, editor_att) =
        channel_state_with(&owner_sk, owner_pk, editor_pk, channel, artifact);
    handle.register_channel_state(artifact, state);

    let mut editor = wait_for_broker(&socket);
    let mut owner = wait_for_broker(&socket);
    let mut observer = wait_for_broker(&socket);
    for s in [&mut editor, &mut owner, &mut observer] {
        write_frame(
            s,
            &CollabRequest::Hello {
                app_id: "test".into(),
                cluster_id: test_cluster(),
                record: None,
                handle: None,
            },
        );
    }
    wait_for_announced(&handle, 3);

    let target = signed_op(
        &editor_sk,
        editor_pk,
        artifact,
        editor_att,
        OpClass::Splice,
        b"target".to_vec(),
        1,
    );
    write_frame(
        &mut editor,
        &CollabRequest::SignedSyncIncremental {
            envelope: Box::new(target.clone()),
        },
    );
    let _ = drain_events(&mut observer, Duration::from_millis(200));

    let first_redact = build_redact_op(
        &owner_sk, owner_pk, artifact, owner_att, &target, "first", 10,
    );
    write_frame(
        &mut owner,
        &CollabRequest::SignedSyncIncremental {
            envelope: Box::new(first_redact.clone()),
        },
    );
    let _ = drain_events(&mut observer, Duration::from_millis(300));

    // Owner attempts to redact the *first Redact* itself.
    let second_redact = build_redact_op(
        &owner_sk,
        owner_pk,
        artifact,
        owner_att,
        &first_redact,
        "redact-of-redact",
        20,
    );
    write_frame(
        &mut owner,
        &CollabRequest::SignedSyncIncremental {
            envelope: Box::new(second_redact),
        },
    );

    // The first Redact's own event already landed above, so filter by
    // seq 20 to isolate whether the recursive one was wrongly fanned out.
    let events = drain_events(&mut observer, Duration::from_millis(400));
    let recursive_fanned = events.iter().any(|e| {
        matches!(
            e,
            CollabEvent::SignedCRDTIncremental { envelope, .. }
                if envelope.op_class == OpClass::Redact && envelope.seq == 20
        )
    });
    assert!(
        !recursive_fanned,
        "recursive Redact (seq 20) must not be fanned out",
    );

    let audit = handle.audit_for(artifact);
    assert!(
        audit.iter().any(|e| matches!(
            e.rejection,
            Some(ValidationError::RecursiveRedaction { .. })
        )),
        "audit must record RecursiveRedaction, got {:?}",
        audit.iter().map(|e| &e.rejection).collect::<Vec<_>>(),
    );

    assert_eq!(
        handle.redactions_for(artifact).len(),
        1,
        "only the non-recursive Redact filed a record",
    );

    drop(editor);
    drop(owner);
    drop(observer);
    drop(handle);
    let _ = std::fs::remove_file(&socket);
}

// Scenario 8: audit entry carries structured fields

#[test]
fn audit_entry_carries_reason_and_redactor_pk() {
    let socket = unique_socket("audit-fields");
    let handle = collab_broker::spawn(socket.clone()).expect("broker bind");

    let (owner_sk, owner_pk) = generate_keypair();
    let (editor_sk, editor_pk) = generate_keypair();
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();
    let (state, owner_att, editor_att) =
        channel_state_with(&owner_sk, owner_pk, editor_pk, channel, artifact);
    handle.register_channel_state(artifact, state);

    let mut editor = wait_for_broker(&socket);
    let mut owner = wait_for_broker(&socket);
    for s in [&mut editor, &mut owner] {
        write_frame(
            s,
            &CollabRequest::Hello {
                app_id: "test".into(),
                cluster_id: test_cluster(),
                record: None,
                handle: None,
            },
        );
    }
    wait_for_announced(&handle, 2);

    let target = signed_op(
        &editor_sk,
        editor_pk,
        artifact,
        editor_att,
        OpClass::Splice,
        b"to-be-redacted".to_vec(),
        1,
    );
    write_frame(
        &mut editor,
        &CollabRequest::SignedSyncIncremental {
            envelope: Box::new(target.clone()),
        },
    );
    let _ = drain_events(&mut owner, Duration::from_millis(200));

    let reason = "ACME-2026-04-17 — GDPR right-to-be-forgotten";
    let redact = build_redact_op(
        &owner_sk, owner_pk, artifact, owner_att, &target, reason, 99,
    );
    let redact_op_for_assert = redact.clone();
    write_frame(
        &mut owner,
        &CollabRequest::SignedSyncIncremental {
            envelope: Box::new(redact),
        },
    );
    let _ = drain_events(&mut editor, Duration::from_millis(300));

    let records = handle.redactions_for(artifact);
    assert_eq!(records.len(), 1);
    let r = &records[0];
    assert_eq!(r.target_op_id, OpId::from_op(&target));
    assert_eq!(r.target_op_hash, hash_target_op(&target).unwrap());
    assert_eq!(r.redactor_pk, owner_pk);
    assert_eq!(r.reason, reason);

    assert_eq!(r.redact_envelope.op_class, OpClass::Redact);
    let parsed = parse_redact_payload(&r.redact_envelope).expect("parse redact");
    assert_eq!(parsed.redacted_by, owner_pk);
    assert_eq!(parsed.reason, reason);
    assert_eq!(
        r.redact_envelope.signature.as_bytes(),
        redact_op_for_assert.signature.as_bytes(),
    );

    drop(editor);
    drop(owner);
    drop(handle);
    let _ = std::fs::remove_file(&socket);
}
