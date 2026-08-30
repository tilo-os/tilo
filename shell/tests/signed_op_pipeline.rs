//! A `SignedOp` envelope flowing through the broker is validated against
//! the per-artifact `ChannelState`: valid envelopes fan out as
//! `CollabEvent::SignedCRDTIncremental`, invalid ones are dropped and land
//! in the audit log. Two scenarios — a registered Editor publishes and the
//! broker forwards; a peer absent from the role table publishes, the
//! broker rejects, no other peer sees the payload.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use collab_fixtures::{role_attestation, signed_op};
use collab_types::{
    ArtifactId, ArtifactKind, AttestationId, ChannelId, ChannelState, OpClass, Role,
    ValidationError, generate_keypair,
};
use shell::shell::collab_broker;
use ui::collaboration::{CollabEvent, CollabRequest};

// Frame helpers — match the broker's wire framing exactly.
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

fn unique_socket(label: &str) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!("qos-broker-{label}-{pid}-{nanos}.sock"))
}

/// The collaboration cluster every client here joins. The broker routes
/// CRDT traffic by the `(app_id, cluster_id)` pair announced on `Hello`,
/// so peers meant to observe each other must announce the same pair.
fn test_cluster() -> uuid::Uuid {
    uuid::Uuid::from_u128(0x3f2b_1c00_0000_4000_8000_0000_0000_0001)
}

/// Block until `n` clients have announced their route — only the `Hello`
/// (not the connect) makes a client addressable, so a send that races it
/// would reach nobody.
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

#[test]
fn valid_signed_op_reaches_other_peers_invalid_one_is_dropped() {
    let socket = unique_socket("valid-fans");
    let handle = collab_broker::spawn(socket.clone()).expect("spawn broker");

    // Owner-trusts-themselves, one Editor attestation for `editor_pk`,
    // artifact known as a Note; `outsider_pk` is *not* in the role table.
    let (owner_sk, owner_pk) = generate_keypair();
    let (editor_sk, editor_pk) = generate_keypair();
    let (outsider_sk, outsider_pk) = generate_keypair();
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();

    let editor_ra = role_attestation(&owner_sk, owner_pk, editor_pk, channel, Role::Editor);
    let editor_att_id = editor_ra.attestation.id;
    let outsider_att_id = AttestationId::new(); // not minted by the channel owner

    let mut state = ChannelState::default();
    state.authorised_signers.push(owner_pk);
    state.artifacts.insert(artifact, ArtifactKind::Note);
    state.register_attestation(editor_ra);
    handle.register_channel_state(artifact, state);

    // editor (sends the valid op), observer (receives), outsider (sends the
    // invalid one).
    let mut editor = wait_for_broker(&socket);
    let mut observer = wait_for_broker(&socket);
    let mut outsider = wait_for_broker(&socket);

    for s in [&mut editor, &mut observer, &mut outsider] {
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

    let valid_op = signed_op(
        &editor_sk,
        editor_pk,
        artifact,
        editor_att_id,
        OpClass::Splice,
        b"hello from a valid peer".to_vec(),
        1,
    );
    write_frame(
        &mut editor,
        &CollabRequest::SignedSyncIncremental {
            envelope: Box::new(valid_op.clone()),
        },
    );

    let event = read_event(&mut observer, Duration::from_millis(500));
    if event.is_none() {
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

    // A forged attestation id that is NOT in the channel's role table.
    let invalid_op = signed_op(
        &outsider_sk,
        outsider_pk,
        artifact,
        outsider_att_id,
        OpClass::Splice,
        b"hello from a valid peer".to_vec(),
        1,
    );
    write_frame(
        &mut outsider,
        &CollabRequest::SignedSyncIncremental {
            envelope: Box::new(invalid_op),
        },
    );

    // Observer must NOT see it — short timeout; anything arriving means the
    // validator failed open (the stream stays connected, so a stray frame surfaces).
    let stray = read_event(&mut observer, Duration::from_millis(200));
    assert!(
        stray.is_none(),
        "observer received an event but the broker should have dropped the invalid op: {stray:?}",
    );

    // The audit log records the matching ValidationError variant.
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

    let editor_ra = role_attestation(&owner_sk, owner_pk, editor_pk, channel, Role::Editor);
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
                cluster_id: test_cluster(),
                record: None,
                handle: None,
            },
        );
    }
    wait_for_announced(&handle, 2);

    let op = signed_op(
        &editor_sk,
        editor_pk,
        artifact,
        editor_att_id,
        OpClass::Splice,
        b"hello from a valid peer".to_vec(),
        1,
    );

    write_frame(
        &mut editor,
        &CollabRequest::SignedSyncIncremental {
            envelope: Box::new(op.clone()),
        },
    );
    let _ = read_event(&mut observer, Duration::from_millis(500))
        .expect("observer should see the first op");

    // Same op, same seq — must be dropped as replay.
    write_frame(
        &mut editor,
        &CollabRequest::SignedSyncIncremental {
            envelope: Box::new(op),
        },
    );
    let stray = read_event(&mut observer, Duration::from_millis(200));
    assert!(
        stray.is_none(),
        "observer received a replayed op — replay defence didn't fire: {stray:?}",
    );

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
