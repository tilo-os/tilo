//! Phase 9 (second half) end-to-end: cooperative cryptographic
//! redaction flowing through the broker.
//!
//! Eight scenarios:
//!
//! 1. `owner_can_redact_a_recent_op` — happy path: owner mints a
//!    Redact, the broker fans it out, the targeted op's payload in
//!    the local log gets rewritten to the marker, and the redaction
//!    is in `redactions_for(artifact)`.
//!
//! 2. `editor_redact_request_is_rejected` — an Editor cannot redact;
//!    the validator's role-table check (step 5) refuses with
//!    `RoleForbidsOpClass`.
//!
//! 3. `redaction_persists_through_audit_log_across_compaction` — a
//!    redaction record survives a subsequent Compaction checkpoint.
//!
//! 4. `redacted_op_is_filtered_from_scrollback` — `FetchScrollback`
//!    no longer surfaces the original payload after the Redact
//!    applies.
//!
//! 5. `late_arriving_op_with_already_redacted_id_is_dropped` — a
//!    buggy peer's re-broadcast of an already-redacted op does NOT
//!    re-introduce the content; the broker pushes a marker entry
//!    and audit-logs `OpAlreadyRedacted` instead of fanning out.
//!
//! 6. `redaction_of_mls_encrypted_op_succeeds` — per the design
//!    doc §5 (option c, "mark redacted, keep ciphertext"): a
//!    Redact targeting an MLS-encrypted op gets through the
//!    validator and applies the redaction.
//!
//! 7. `recursive_redaction_is_rejected` — a Redact whose
//!    `target_op_id` points to another Redact is rejected with
//!    `RecursiveRedaction`.
//!
//! 8. `audit_entry_carries_reason_and_redactor_pk` — verifies
//!    `BrokerHandle::redactions_for` returns the structured record
//!    with the expected fields.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use collab_types::{
    ArtifactId, ArtifactKind, Attestation, AttestationId, ChannelId, ChannelState,
    CompactionCheckpoint, Ed25519PublicKey, EncryptionMetadata, OpClass, OpId, RedactRequest,
    RedactedMarker, Role, RoleAttestation, Signature, SignedOp, ValidationError,
    encode_compaction_payload, encode_redact_payload, generate_keypair, hash_op_prefix,
    hash_target_op, parse_redact_payload, sign_event, sign_op,
};
use collab_fixtures::{role_attestation, signed_op, signed_op_encrypted};
use ed25519_dalek::SigningKey;
use shell::shell::collab_broker;
use ui::collaboration::{CollabEvent, CollabRequest};

// ---------- frame helpers ----------

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
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("qos-broker-rd-{label}-{pid}-{nanos}.sock"))
}

/// The collaboration cluster every client in these scenarios joins.
///
/// The broker routes CRDT traffic by the `(app_id, cluster_id)` pair a
/// client announces on its `Hello`, so peers that are meant to observe
/// each other's edits must announce the same pair — these scenarios
/// pair this cluster with a single app id per file. A per-client
/// random uuid would put every client in a cluster of its own, which
/// is how a real deployment keeps unrelated documents apart.
fn test_cluster() -> uuid::Uuid {
    uuid::Uuid::from_u128(0x3f2b_1c00_0000_4000_8000_0000_0000_0001)
}

/// Block until `n` clients have announced their route.
///
/// Connecting and saying `Hello` are separate steps, and only the
/// second makes a client addressable — the broker routes CRDT traffic
/// by the announced `(app_id, cluster_id)` pair. A send that races
/// another client's `Hello` would reach nobody.
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

// ---------- attestation / op factories ----------

/// Build a channel state populated with `owner_pk` as the
/// authorised-signers list, plus an Owner attestation for `owner_pk`
/// and an Editor attestation for `editor_pk`. Returns (state,
/// owner_attestation_id, editor_attestation_id).
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

/// Build a Redact op envelope that targets `target_op` with the
/// given reason. The envelope is signed by `owner_sk` and references
/// `owner_att`.
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

// =====================================================================
// Scenario 1: happy path
// =====================================================================

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

    // Editor publishes one op carrying a secret payload.
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

    // Owner mints a Redact targeting the editor's op.
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
    // The Redact fan-out reaches the observer.
    let fanned = drain_events(&mut observer, Duration::from_millis(400));
    assert!(
        fanned.iter().any(|e| matches!(
            e,
            CollabEvent::SignedCRDTIncremental { envelope, .. } if envelope.op_class == OpClass::Redact
        )),
        "observer must see the Redact event fan out",
    );

    // The broker now reports the redaction structurally.
    let records = handle.redactions_for(artifact);
    assert_eq!(records.len(), 1, "exactly one redaction recorded");
    let rec = &records[0];
    assert_eq!(rec.target_op_id, OpId::from_op(&target_op));
    assert_eq!(rec.redactor_pk, owner_pk);
    assert_eq!(rec.reason, "GDPR request from data subject");

    // Scrollback for the editor's seq no longer carries the secret bytes.
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
                    // It IS the canonical marker payload.
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

// =====================================================================
// Scenario 2: editor cannot redact
// =====================================================================

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

    // Craft a target op (unused, but lets us build a well-formed Redact).
    let target = signed_op(
        &editor_sk,
        editor_pk,
        artifact,
        editor_att,
        OpClass::Splice,
        b"victim".to_vec(),
        1,
    );

    // Editor tries to redact it themselves.
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

    // The validator dropped it with RoleForbidsOpClass.
    let audit = handle.audit_for(artifact);
    assert!(
        audit
            .iter()
            .any(|e| matches!(e.rejection, Some(ValidationError::RoleForbidsOpClass))),
        "audit must record RoleForbidsOpClass, got {:?}",
        audit.iter().map(|e| &e.rejection).collect::<Vec<_>>(),
    );
    // And no redaction record was filed.
    assert!(handle.redactions_for(artifact).is_empty());

    drop(editor);
    drop(observer);
    drop(handle);
    let _ = std::fs::remove_file(&socket);
}

// =====================================================================
// Scenario 3: redaction persists across compaction
// =====================================================================

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

    // Editor publishes 3 ops.
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

    // Owner redacts op #2 (the middle one).
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

    // Owner now emits a Compaction covering ops 1..=3. The prefix
    // hash must be computed over the **original** envelope bytes
    // (design doc §4), not the post-redaction marker bytes.
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

    // The redaction record still exists after compaction.
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

// =====================================================================
// Scenario 4: redacted op is filtered from scrollback
// =====================================================================

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

    // Observer fetches scrollback for the targeted seq range.
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

// =====================================================================
// Scenario 5: late-arriving already-redacted op is dropped
// =====================================================================

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

    // Build an op (the redaction target). DO NOT push it through the
    // broker yet — we redact it first so we can simulate "late
    // arrival from a peer who never got the Redact".
    let target = signed_op(
        &editor_sk,
        editor_pk,
        artifact,
        editor_att,
        OpClass::Splice,
        b"sensitive content".to_vec(),
        3,
    );

    // Owner pre-emptively redacts the op (the redaction reached this
    // broker first; the original op hasn't arrived yet).
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

    // Now the "buggy peer" re-broadcasts the original (pre-redaction)
    // envelope — possibly because their network was offline and they
    // queued it.
    write_frame(
        &mut buggy_peer,
        &CollabRequest::SignedSyncIncremental {
            envelope: Box::new(target.clone()),
        },
    );

    // The observer must NOT receive the original payload — the broker
    // refuses to fan a late-arrival of a redacted op.
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

    // The audit log records OpAlreadyRedacted.
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

// =====================================================================
// Scenario 6: redaction of MLS-encrypted op succeeds
// =====================================================================

#[test]
fn redaction_of_mls_encrypted_op_succeeds() {
    // Per design doc §5, option (c): a Redact targeting an op whose
    // payload is MLS ciphertext does NOT decrypt. The validator's
    // role table + redaction-aware step both apply regardless of
    // encryption. The audit log records the redaction; the op-log
    // entry's payload is rewritten to the marker (the original
    // ciphertext is preserved in `original_envelopes` so the audit
    // path can re-verify the original `target_op_hash`).
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

    // Editor publishes an op whose payload is "ciphertext" (we fake
    // the ciphertext with arbitrary bytes — the broker treats
    // ciphertext opaquely; it never decrypts inside validation).
    let ciphertext = b"\x00\x01\xfe\xff...mls-ciphertext-blob".to_vec();
    let target = signed_op_encrypted(
        &editor_sk,
        editor_pk,
        artifact,
        editor_att,
        OpClass::Splice,
        ciphertext.clone(),
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

// =====================================================================
// Scenario 7: recursive redaction is rejected
// =====================================================================

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

    // 1) Editor publishes a Splice op.
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

    // 2) Owner redacts the Splice. This Redact lands in the op log.
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

    // 3) Owner attempts to redact the *first Redact* itself.
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

    // The observer should NOT receive a second SignedCRDTIncremental
    // event from the recursive Redact. The first one DOES still land
    // here from step (2), so we look at events arriving AFTER step (3).
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

    // Audit log records RecursiveRedaction.
    let audit = handle.audit_for(artifact);
    assert!(
        audit.iter().any(|e| matches!(
            e.rejection,
            Some(ValidationError::RecursiveRedaction { .. })
        )),
        "audit must record RecursiveRedaction, got {:?}",
        audit.iter().map(|e| &e.rejection).collect::<Vec<_>>(),
    );

    // The first redaction record is still the only one filed.
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

// =====================================================================
// Scenario 8: audit entry carries structured fields
// =====================================================================

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

    // The redact_envelope copy verifies as a well-formed Redact.
    assert_eq!(r.redact_envelope.op_class, OpClass::Redact);
    let parsed = parse_redact_payload(&r.redact_envelope).expect("parse redact");
    assert_eq!(parsed.redacted_by, owner_pk);
    assert_eq!(parsed.reason, reason);
    // The captured envelope's signature is the same one minted by the owner.
    assert_eq!(
        r.redact_envelope.signature.as_bytes(),
        redact_op_for_assert.signature.as_bytes(),
    );

    drop(editor);
    drop(owner);
    drop(handle);
    let _ = std::fs::remove_file(&socket);
}
