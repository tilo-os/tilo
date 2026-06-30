//! Phase 3 end-to-end: a `SignedOp` envelope flowing through the
//! broker is validated against the per-artifact `ChannelState`. Valid
//! envelopes fan out as `CollabEvent::SignedCRDTIncremental`; invalid
//! envelopes are dropped and land in the audit log.
//!
//! This is the headline test the spec's Phase 3 promises: "the
//! protocol actually enforces who's allowed to edit". The two
//! scenarios exercised here are
//!
//! 1. A peer with a registered Editor attestation publishes an op →
//!    the broker accepts and forwards it.
//! 2. A peer with NO attestation in the channel's role table
//!    publishes an op → the broker rejects, no other peer sees the
//!    payload, the audit log records the rejection.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use ed25519_dalek::SigningKey;
use qos_collab_types::{
    ArtifactId, ArtifactKind, Attestation, AttestationId, ChannelId, ChannelState, Ed25519PublicKey,
    OpClass, Role, RoleAttestation, Signature, SignedOp, ValidationError, generate_keypair,
    sign_event, sign_op,
};
use shell::shell::collab_broker;
use ui::collaboration::{CollabEvent, CollabRequest};

/// Frame helpers — match the broker's wire framing exactly.
fn write_frame<T: serde::Serialize>(stream: &mut UnixStream, v: &T) {
    let bytes = serde_json::to_vec(v).expect("serialize");
    let len = (bytes.len() as u32).to_le_bytes();
    stream.write_all(&len).expect("write len");
    stream.write_all(&bytes).expect("write body");
}

fn read_event(stream: &mut UnixStream, timeout: Duration) -> Option<CollabEvent> {
    stream
        .set_read_timeout(Some(timeout))
        .expect("set timeout");
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).ok()?;
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn unique_socket(label: &str) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("qos-broker-{label}-{pid}-{nanos}.sock"))
}

fn make_role_attestation(
    issuer_sk: &SigningKey,
    issuer_pk: Ed25519PublicKey,
    subject: Ed25519PublicKey,
    channel: ChannelId,
    role: Role,
) -> RoleAttestation {
    // Validity window: from a year ago to ten years from now. The op
    // signing timestamp is `SystemTime::now()`, so the window must
    // straddle it.
    let now = SystemTime::now();
    let mut att = Attestation {
        id: AttestationId::new(),
        issued_by: issuer_pk,
        subject,
        claims: vec![],
        valid_from: now - Duration::from_secs(365 * 24 * 3600),
        valid_until: now + Duration::from_secs(10 * 365 * 24 * 3600),
        signature: Signature::from_bytes([0u8; 64]),
    };
    let sb = att.signing_bytes().expect("attestation signing_bytes");
    att.signature = sign_event(&sb, issuer_sk).expect("sign attestation");
    RoleAttestation {
        channel,
        role,
        attestation: att,
    }
}

fn build_signed_op(
    author_sk: &SigningKey,
    author_pk: Ed25519PublicKey,
    artifact: ArtifactId,
    attestation_id: AttestationId,
    seq: u64,
) -> SignedOp {
    let mut op = SignedOp::new_unsigned(
        b"hello from a valid peer".to_vec(),
        artifact,
        author_pk,
        attestation_id,
        OpClass::Splice,
        SystemTime::now(),
        seq,
    );
    sign_op(&mut op, author_sk).expect("sign op");
    op
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

#[test]
fn valid_signed_op_reaches_other_peers_invalid_one_is_dropped() {
    let socket = unique_socket("valid-fans");
    let handle = collab_broker::spawn(socket.clone()).expect("spawn broker");

    // Channel state: owner-trusts-themselves, one Editor attestation
    // for `editor_pk`, artifact known as a Note. A second peer
    // `outsider_pk` is *not* in the role table.
    let (owner_sk, owner_pk) = generate_keypair();
    let (editor_sk, editor_pk) = generate_keypair();
    let (outsider_sk, outsider_pk) = generate_keypair();
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();

    let editor_ra = make_role_attestation(&owner_sk, owner_pk, editor_pk, channel, Role::Editor);
    let editor_att_id = editor_ra.attestation.id;
    let outsider_att_id = AttestationId::new(); // not minted by the channel owner

    let mut state = ChannelState::default();
    state.authorised_signers.push(owner_pk);
    state.artifacts.insert(artifact, ArtifactKind::Note);
    state.register_attestation(editor_ra);
    handle.register_channel_state(artifact, state);

    // Three peers connect: editor (sender of valid op), observer
    // (receiver), outsider (sender of invalid op).
    let mut editor = wait_for_broker(&socket);
    let mut observer = wait_for_broker(&socket);
    let mut outsider = wait_for_broker(&socket);

    for s in [&mut editor, &mut observer, &mut outsider] {
        write_frame(
            s,
            &CollabRequest::Hello {
                app_id: "test".into(),
                cluster_id: uuid::Uuid::new_v4(),
            },
        );
    }

    // Editor publishes a valid signed op.
    let valid_op = build_signed_op(&editor_sk, editor_pk, artifact, editor_att_id, 1);
    write_frame(
        &mut editor,
        &CollabRequest::SignedSyncIncremental {
            envelope: Box::new(valid_op.clone()),
        },
    );

    // Observer should see it as a SignedCRDTIncremental.
    let event = read_event(&mut observer, Duration::from_millis(500));
    if event.is_none() {
        // Diagnostic: print the audit log to see why the valid op was dropped.
        let audit = handle.audit_for(artifact);
        panic!(
            "observer should have received the valid op. \
             audit log has {} entries: {:?}",
            audit.len(),
            audit.iter().map(|e| &e.rejection).collect::<Vec<_>>()
        );
    }
    let event = event.unwrap();
    match event {
        CollabEvent::SignedCRDTIncremental { envelope, .. } => {
            assert_eq!(envelope.payload, valid_op.payload, "payload preserved");
            assert_eq!(envelope.seq, 1);
        }
        other => panic!("expected SignedCRDTIncremental, got {other:?}"),
    }

    // Outsider tries to publish an op with a forged attestation id
    // that is NOT in the channel's role table.
    let invalid_op = build_signed_op(&outsider_sk, outsider_pk, artifact, outsider_att_id, 1);
    write_frame(
        &mut outsider,
        &CollabRequest::SignedSyncIncremental {
            envelope: Box::new(invalid_op.clone()),
        },
    );

    // Observer should NOT see the invalid op. Reading with a short
    // timeout — if anything comes through, the validator failed
    // open. The observer's stream stays connected so a stray frame
    // would surface here.
    let stray = read_event(&mut observer, Duration::from_millis(200));
    assert!(
        stray.is_none(),
        "observer received an event but the broker should have dropped the invalid op: {stray:?}",
    );

    // The audit log records the rejection with the matching
    // ValidationError variant.
    let audit = handle.audit_for(artifact);
    assert_eq!(audit.len(), 1, "exactly one rejection logged");
    let entry = &audit[0];
    assert_eq!(
        entry.envelope.author, outsider_pk,
        "logged the outsider's envelope, not anyone else's",
    );
    match entry.rejection.as_ref().expect("entry is a rejection") {
        ValidationError::UnknownAttestation(id) => {
            assert_eq!(*id, outsider_att_id, "logged the forged attestation id");
        }
        other => panic!(
            "expected ValidationError::UnknownAttestation, got {other:?} — \
             the validator should have stopped at step 1"
        ),
    }

    // Cleanup.
    drop(editor);
    drop(observer);
    drop(outsider);
    drop(handle);
    let _ = std::fs::remove_file(&socket);
}

#[test]
fn replay_of_same_signed_op_is_dropped() {
    let socket = unique_socket("replay");
    let handle = collab_broker::spawn(socket.clone()).expect("spawn broker");

    let (owner_sk, owner_pk) = generate_keypair();
    let (editor_sk, editor_pk) = generate_keypair();
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();

    let editor_ra = make_role_attestation(&owner_sk, owner_pk, editor_pk, channel, Role::Editor);
    let editor_att_id = editor_ra.attestation.id;

    let mut state = ChannelState::default();
    state.authorised_signers.push(owner_pk);
    state.artifacts.insert(artifact, ArtifactKind::Note);
    state.register_attestation(editor_ra);
    handle.register_channel_state(artifact, state);

    let mut editor = wait_for_broker(&socket);
    let mut observer = wait_for_broker(&socket);

    for s in [&mut editor, &mut observer] {
        write_frame(
            s,
            &CollabRequest::Hello {
                app_id: "test".into(),
                cluster_id: uuid::Uuid::new_v4(),
            },
        );
    }

    let op = build_signed_op(&editor_sk, editor_pk, artifact, editor_att_id, 1);

    // First send — valid, observer sees it.
    write_frame(
        &mut editor,
        &CollabRequest::SignedSyncIncremental {
            envelope: Box::new(op.clone()),
        },
    );
    let _ = read_event(&mut observer, Duration::from_millis(500))
        .expect("observer should see the first op");

    // Second send — same op, same seq — must be dropped as replay.
    write_frame(
        &mut editor,
        &CollabRequest::SignedSyncIncremental {
            envelope: Box::new(op.clone()),
        },
    );
    let stray = read_event(&mut observer, Duration::from_millis(200));
    assert!(
        stray.is_none(),
        "observer received a replayed op — replay defence didn't fire: {stray:?}",
    );

    // The audit log records exactly one rejection (the replay).
    let audit = handle.audit_for(artifact);
    let replays: Vec<_> = audit
        .iter()
        .filter(|e| matches!(e.rejection, Some(ValidationError::ReplaySeq { .. })))
        .collect();
    assert_eq!(
        replays.len(),
        1,
        "exactly one replay rejection should be logged, got {:?}",
        audit.iter().map(|e| &e.rejection).collect::<Vec<_>>()
    );

    drop(editor);
    drop(observer);
    drop(handle);
    let _ = std::fs::remove_file(&socket);
}
