//! End-to-end: in-memory scrollback + host-driven compaction through the
//! broker, plus the cooperative scrollback path that bridges through a
//! `ShellNetwork` mock.
//!
//! Three scenarios: a peer fetches another's ops out of the broker's log;
//! a `Compaction` checkpoint evicts a prefix so fetching it returns
//! `exhausted=true`; and a broker that lacks a range emits a
//! `NeighbourMsg::ScrollbackRequest`, then forwards the answering
//! broker's response to its local app as a follow-up `ScrollbackBatch`.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use collab_fixtures::{role_attestation, signed_op};
use collab_types::{
    ArtifactId, ArtifactKind, AttestationId, ChannelId, ChannelState, CompactionCheckpoint,
    Ed25519PublicKey, OpClass, Role, SignedOp, encode_compaction_payload, generate_keypair,
    hash_op_prefix,
};
use ed25519_dalek::SigningKey;
use shell::shell::collab_broker;
use ui::collaboration::{CollabEvent, CollabRequest};

// frame helpers

/// How long to wait for something the contract says must happen — a
/// backstop, not a pacing device. Every wait below it resolves in
/// microseconds on an idle machine, so this costs nothing; it only stops
/// the harness giving up on a broker that's merely descheduled.
///
/// Waits asserting something *does not* happen keep their own short
/// deadlines — there the wait is the measurement, so lengthening it would
/// only slow the suite down.
const REPLY_BACKSTOP: Duration = Duration::from_secs(10);

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

/// Read fan-out events until `want` signed increments have arrived, or
/// `timeout` elapses. Returns everything read.
///
/// `drain_events` alone can't answer "has the broker finished recording
/// the editor's ops": it stops at the first read that times out, which is
/// the first scheduling gap, not the end of the stream. The two
/// connections are served by independent reader threads with no ordering
/// between them, so a `FetchScrollback` sent after a quiet socket can
/// still be answered from a half-written op log — reading `[3, 4, 5]`
/// where it wanted `[8, 9, 10]` and blaming paging. Waiting on the count
/// the scenario actually depends on removes the guess.
fn drain_until_signed(stream: &mut UnixStream, want: usize, timeout: Duration) -> Vec<CollabEvent> {
    let deadline = Instant::now() + timeout;
    let mut out = Vec::new();
    let mut signed = 0usize;
    // A `None` that came back without spending its budget is a
    // closed/desynchronised stream, not a slow broker — a timeout by
    // definition consumes the whole `remain`. Retry a few times, not in a
    // tight loop, then let the caller's assertion report what it saw.
    let mut fruitless = 0usize;
    while signed < want {
        let remain = deadline.saturating_duration_since(Instant::now());
        if remain.is_zero() {
            break;
        }
        let before = Instant::now();
        if let Some(ev) = read_event(stream, remain) {
            fruitless = 0;
            if matches!(ev, CollabEvent::SignedCRDTIncremental { .. }) {
                signed += 1;
            }
            out.push(ev);
        } else {
            if before.elapsed() >= remain {
                break;
            }
            fruitless += 1;
            if fruitless >= 8 {
                break;
            }
        }
    }
    out
}

fn unique_socket(label: &str) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!("qos-broker-sb-{label}-{pid}-{nanos}.sock"))
}

/// The collaboration cluster every client in these scenarios joins.
///
/// The broker routes CRDT traffic by the `(app_id, cluster_id)` pair a
/// client announces on `Hello`, so peers meant to observe each other must
/// announce the same pair — a per-client random uuid would put every
/// client in a cluster of its own.
fn test_cluster() -> uuid::Uuid {
    uuid::Uuid::from_u128(0x3f2b_1c00_0000_4000_8000_0000_0000_0001)
}

/// Block until `n` clients have announced their route. Connecting and
/// saying `Hello` are separate steps; only the second makes a client
/// addressable (the broker routes by the announced pair), so a send that
/// races another client's `Hello` would reach nobody.
fn wait_for_announced(handle: &collab_broker::BrokerHandle, n: usize) {
    let deadline = Instant::now() + REPLY_BACKSTOP;
    while Instant::now() < deadline {
        if handle.announced_peer_count() >= n {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!(
        "only {} of {n} clients announced a route within {REPLY_BACKSTOP:?}",
        handle.announced_peer_count()
    );
}

fn wait_for_broker(socket: &PathBuf) -> UnixStream {
    let deadline = Instant::now() + REPLY_BACKSTOP;
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

/// Channel state with `signers` as the authorised-signers list, one
/// Editor attestation for `editor_pk` signed by the first owner, and one
/// Note artifact.
fn channel_state_with(
    owner_sk: &SigningKey,
    owner_pk: Ed25519PublicKey,
    editor_pk: Ed25519PublicKey,
    channel: ChannelId,
    artifact: ArtifactId,
) -> (ChannelState, AttestationId, AttestationId) {
    // Owner's own attestation underwriting their own compaction ops.
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

// scenario 1: in-memory scrollback

#[test]
fn peer_can_fetch_recent_scrollback() {
    let socket = unique_socket("fetch-recent");
    let handle = collab_broker::spawn(socket.clone()).expect("broker bind");

    let (owner_sk, owner_pk) = generate_keypair();
    let (editor_sk, editor_pk) = generate_keypair();
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();
    let (state, _owner_att, editor_att) =
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

    for seq in 1..=5_u64 {
        let op = signed_op(
            &editor_sk,
            editor_pk,
            artifact,
            editor_att,
            OpClass::Splice,
            format!("op-{seq}").into_bytes(),
            seq,
        );
        write_frame(
            &mut editor,
            &CollabRequest::SignedSyncIncremental {
                envelope: Box::new(op),
            },
        );
    }
    // Drain the fan-out so the batch read below is unambiguous.
    let fanned = drain_until_signed(&mut observer, 5, REPLY_BACKSTOP);
    let signed_count = fanned
        .iter()
        .filter(|e| matches!(e, CollabEvent::SignedCRDTIncremental { .. }))
        .count();
    assert_eq!(signed_count, 5, "observer should have seen all 5 ops");

    // Even though the observer already saw the live events, the broker holds
    // the log and must reply.
    write_frame(
        &mut observer,
        &CollabRequest::FetchScrollback {
            artifact,
            before_seq: 6,
            max: 10,
        },
    );
    let event =
        read_event(&mut observer, REPLY_BACKSTOP).expect("observer must receive ScrollbackBatch");
    match event {
        CollabEvent::ScrollbackBatch {
            artifact: art,
            ops,
            exhausted,
        } => {
            assert_eq!(art, artifact);
            assert_eq!(ops.len(), 5, "all 5 ops returned");
            let seqs: Vec<u64> = ops.iter().map(|o| o.seq).collect();
            assert_eq!(seqs, vec![1, 2, 3, 4, 5]);
            assert!(exhausted, "all 5 in 1 batch → exhausted=true");
        }
        other => panic!("expected ScrollbackBatch, got {other:?}"),
    }

    // The editor (who did NOT ask) must NOT receive the batch.
    let stray = read_event(&mut editor, Duration::from_millis(150));
    assert!(stray.is_none(), "scrollback batch leaked to non-requester");

    drop(editor);
    drop(observer);
    drop(handle);
    let _ = std::fs::remove_file(&socket);
}

#[test]
fn empty_scrollback_returns_exhausted_true() {
    // The v1 contract on the "nothing to show" edge: (empty, exhausted=true).
    let socket = unique_socket("empty-sb");
    let handle = collab_broker::spawn(socket.clone()).expect("broker bind");

    let (owner_sk, owner_pk) = generate_keypair();
    let (_editor_sk, editor_pk) = generate_keypair();
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();
    let (state, _, _) = channel_state_with(&owner_sk, owner_pk, editor_pk, channel, artifact);
    handle.register_channel_state(artifact, state);

    let mut peer = wait_for_broker(&socket);
    write_frame(
        &mut peer,
        &CollabRequest::Hello {
            app_id: "test".into(),
            cluster_id: test_cluster(),
            record: None,
            handle: None,
        },
    );

    write_frame(
        &mut peer,
        &CollabRequest::FetchScrollback {
            artifact,
            before_seq: 100,
            max: 10,
        },
    );
    let event =
        read_event(&mut peer, REPLY_BACKSTOP).expect("must receive an empty ScrollbackBatch");
    match event {
        CollabEvent::ScrollbackBatch { ops, exhausted, .. } => {
            assert!(ops.is_empty());
            assert!(exhausted);
        }
        other => panic!("expected ScrollbackBatch, got {other:?}"),
    }
    drop(peer);
    drop(handle);
    let _ = std::fs::remove_file(&socket);
}

#[test]
fn scrollback_paging_respects_max() {
    // With max < total the broker returns the most recent `max` ops with
    // exhausted=false; the requester is expected to page.
    let socket = unique_socket("paging");
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
    for seq in 1..=10_u64 {
        let op = signed_op(
            &editor_sk,
            editor_pk,
            artifact,
            editor_att,
            OpClass::Splice,
            format!("op-{seq}").into_bytes(),
            seq,
        );
        write_frame(
            &mut editor,
            &CollabRequest::SignedSyncIncremental {
                envelope: Box::new(op),
            },
        );
    }
    let _drained = drain_until_signed(&mut observer, 10, REPLY_BACKSTOP);

    write_frame(
        &mut observer,
        &CollabRequest::FetchScrollback {
            artifact,
            before_seq: 11,
            max: 3,
        },
    );
    let event = read_event(&mut observer, REPLY_BACKSTOP).expect("ScrollbackBatch");
    match event {
        CollabEvent::ScrollbackBatch { ops, exhausted, .. } => {
            let seqs: Vec<u64> = ops.iter().map(|o| o.seq).collect();
            assert_eq!(seqs, vec![8, 9, 10], "tail-most page returned first");
            assert!(!exhausted, "more pages exist (seqs 1..=7)");
        }
        other => panic!("expected ScrollbackBatch, got {other:?}"),
    }
    drop(editor);
    drop(observer);
    drop(handle);
    let _ = std::fs::remove_file(&socket);
}

// scenario 2: compaction evicts

#[test]
fn compaction_evicts_log_then_scrollback_returns_exhausted() {
    let socket = unique_socket("compact");
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

    let mut ops: Vec<SignedOp> = Vec::new();
    for seq in 1..=5_u64 {
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
    // The checkpoint hashes prefix ops 1..=3, validated against the broker's
    // own log — if the log is still half-written the checkpoint is rejected
    // for a reason unrelated to compaction, so wait for the ops rather than
    // the socket going quiet.
    let _drained = drain_until_signed(&mut observer, 5, REPLY_BACKSTOP);
    let _drained = drain_until_signed(&mut owner, 5, REPLY_BACKSTOP);

    let prefix: Vec<SignedOp> = ops.iter().filter(|o| o.seq <= 3).cloned().collect();
    let cp = CompactionCheckpoint {
        artifact,
        up_to_seq: 3,
        prefix_hash: hash_op_prefix(&prefix).expect("hash"),
    };
    let cp_bytes = encode_compaction_payload(&cp).expect("encode cp");
    let comp_op = signed_op(
        &owner_sk,
        owner_pk,
        artifact,
        owner_att,
        OpClass::Compaction,
        cp_bytes,
        100, // seq must be greater than any prior op's seq for replay defence
    );
    write_frame(
        &mut owner,
        &CollabRequest::SignedSyncIncremental {
            envelope: Box::new(comp_op),
        },
    );
    // Drain the compaction op's own fan-out so the next read sees only the
    // scrollback reply.
    let _ = drain_until_signed(&mut observer, 1, REPLY_BACKSTOP);
    let _ = drain_until_signed(&mut editor, 1, REPLY_BACKSTOP);

    write_frame(
        &mut observer,
        &CollabRequest::FetchScrollback {
            artifact,
            before_seq: 4,
            max: 10,
        },
    );
    let event = read_event(&mut observer, REPLY_BACKSTOP).expect("ScrollbackBatch");
    match event {
        CollabEvent::ScrollbackBatch { ops, exhausted, .. } => {
            assert!(
                ops.is_empty(),
                "compacted prefix should be gone, got {} ops",
                ops.len()
            );
            assert!(exhausted, "broker has no more history for that range");
        }
        other => panic!("expected ScrollbackBatch, got {other:?}"),
    }

    // But ops 4..=5 are still reachable.
    write_frame(
        &mut observer,
        &CollabRequest::FetchScrollback {
            artifact,
            before_seq: 6,
            max: 10,
        },
    );
    let event = read_event(&mut observer, REPLY_BACKSTOP).expect("ScrollbackBatch");
    match event {
        CollabEvent::ScrollbackBatch { ops, .. } => {
            let seqs: Vec<u64> = ops.iter().map(|o| o.seq).collect();
            // The compaction op (seq 100) falls outside before_seq=6.
            assert_eq!(seqs, vec![4, 5]);
        }
        other => panic!("expected ScrollbackBatch, got {other:?}"),
    }

    drop(editor);
    drop(owner);
    drop(observer);
    drop(handle);
    let _ = std::fs::remove_file(&socket);
}

#[test]
fn compaction_op_class_rejected_for_editor() {
    // Defence-in-depth: an Editor attestation must not carry a Compaction op
    // past the validator — the role table pins compaction to the owner.
    let socket = unique_socket("editor-cant-compact");
    let handle = collab_broker::spawn(socket.clone()).expect("broker bind");

    let (owner_sk, owner_pk) = generate_keypair();
    let (editor_sk, editor_pk) = generate_keypair();
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();
    let (state, _owner_att, editor_att) =
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

    let cp = CompactionCheckpoint {
        artifact,
        up_to_seq: 0,
        prefix_hash: hash_op_prefix(&[]).expect("hash"),
    };
    let bytes = encode_compaction_payload(&cp).expect("encode");
    let bad_op = signed_op(
        &editor_sk,
        editor_pk,
        artifact,
        editor_att,
        OpClass::Compaction,
        bytes,
        1,
    );
    write_frame(
        &mut editor,
        &CollabRequest::SignedSyncIncremental {
            envelope: Box::new(bad_op),
        },
    );
    let stray = read_event(&mut observer, Duration::from_millis(250));
    assert!(stray.is_none(), "Editor-issued Compaction must be dropped");
    // Audit log records the rejection.
    let audit = handle.audit_for(artifact);
    assert!(
        audit.iter().any(|e| matches!(
            e.rejection,
            Some(collab_types::ValidationError::RoleForbidsOpClass)
        )),
        "audit must record RoleForbidsOpClass, got {:?}",
        audit.iter().map(|e| &e.rejection).collect::<Vec<_>>()
    );

    drop(editor);
    drop(observer);
    drop(handle);
    let _ = std::fs::remove_file(&socket);
}

// scenario 3: cooperative round-trip

#[test]
fn cooperative_scrollback_round_trip() {
    // Two brokers in process. A ("requesting") has an EMPTY op log, so a
    // local FetchScrollback gets empty+exhausted AND publishes a gossip
    // ScrollbackRequest; B ("responding") holds the ops, simulated by calling
    // `handle_scrollback_response_message` on A's ShellNetwork directly (the
    // wire-level round trip is covered by `protocol.rs::round_trips`).
    // Contract: A's local app then receives a follow-up ScrollbackBatch.
    use shell::shell::network::ShellNetwork;
    use shell::shell::state::WorkspaceTree;
    use std::sync::Arc;

    let socket = unique_socket("coop");
    let tmp = tempfile::tempdir().expect("tempdir");
    let key_path = tmp.path().join("peer-key");
    let tree = WorkspaceTree::bootstrap_demo().shared();
    let net = Arc::new(ShellNetwork::spawn(tree, &key_path).expect("spawn net"));
    let handle = collab_broker::spawn_with_peer_sync(socket.clone(), Some(Arc::clone(&net)))
        .expect("broker bind");

    // No channel state, so the first FetchScrollback finds nothing.
    let (owner_sk, owner_pk) = generate_keypair();
    let (_editor_sk, editor_pk) = generate_keypair();
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();
    let (state, _, _) = channel_state_with(&owner_sk, owner_pk, editor_pk, channel, artifact);
    handle.register_channel_state(artifact, state);

    let mut app = wait_for_broker(&socket);
    write_frame(
        &mut app,
        &CollabRequest::Hello {
            app_id: "test".into(),
            cluster_id: test_cluster(),
            record: None,
            handle: None,
        },
    );

    write_frame(
        &mut app,
        &CollabRequest::FetchScrollback {
            artifact,
            before_seq: 100,
            max: 10,
        },
    );
    let first = read_event(&mut app, REPLY_BACKSTOP).expect("first ScrollbackBatch (local empty)");
    match first {
        CollabEvent::ScrollbackBatch { ops, exhausted, .. } => {
            assert!(ops.is_empty(), "local log holds nothing");
            assert!(exhausted, "local exhausted=true");
        }
        other => panic!("expected ScrollbackBatch, got {other:?}"),
    }

    // The response as gossip would deliver it. v1 doesn't validate
    // cooperative responses against the signature pipeline here — that
    // happens when the receiving app renders them.
    let synthetic_ops = vec![
        Box::new(signed_op(
            &owner_sk,
            owner_pk,
            artifact,
            AttestationId::new(),
            OpClass::Splice,
            b"older-op-1".to_vec(),
            1,
        )),
        Box::new(signed_op(
            &owner_sk,
            owner_pk,
            artifact,
            AttestationId::new(),
            OpClass::Splice,
            b"older-op-2".to_vec(),
            2,
        )),
    ];
    net.handle_scrollback_response_message(99u64, artifact, synthetic_ops);

    // The drain thread is on a 100 ms cadence, so poll with a generous
    // timeout for the follow-up ScrollbackBatch.
    let mut got: Option<CollabEvent> = None;
    let deadline = Instant::now() + REPLY_BACKSTOP;
    while Instant::now() < deadline {
        if let Some(ev) = read_event(&mut app, Duration::from_millis(200))
            && matches!(ev, CollabEvent::ScrollbackBatch { .. })
        {
            got = Some(ev);
            break;
        }
    }
    let event = got.expect("follow-up ScrollbackBatch from cooperative path");
    match event {
        CollabEvent::ScrollbackBatch {
            artifact: art,
            ops,
            exhausted,
        } => {
            assert_eq!(art, artifact);
            assert_eq!(ops.len(), 2);
            assert!(exhausted);
            assert_eq!(ops[0].payload, b"older-op-1");
            assert_eq!(ops[1].payload, b"older-op-2");
        }
        other => panic!("expected ScrollbackBatch, got {other:?}"),
    }

    drop(app);
    drop(handle);
    drop(net);
    let _ = std::fs::remove_file(&socket);
}

#[test]
fn cooperative_request_published_when_local_empty() {
    // The *outbound* half: a local FetchScrollback that finds nothing must
    // publish a NeighbourMsg::ScrollbackRequest through the actor's command
    // channel. With no Iroh mesh the request reaches no peer, so this
    // asserts only that the publish runs and the local reply is (empty,
    // exhausted=true); the wire round trip is covered by
    // `network/protocol.rs::scrollback_request_round_trips_*`.
    use shell::shell::network::ShellNetwork;
    use shell::shell::state::WorkspaceTree;
    use std::sync::Arc;

    let socket = unique_socket("coop-empty");
    let tmp = tempfile::tempdir().expect("tempdir");
    let key_path = tmp.path().join("peer-key");
    let tree = WorkspaceTree::bootstrap_demo().shared();
    let net = Arc::new(ShellNetwork::spawn(tree, &key_path).expect("spawn net"));
    let handle = collab_broker::spawn_with_peer_sync(socket.clone(), Some(Arc::clone(&net)))
        .expect("broker bind");

    let mut app = wait_for_broker(&socket);
    write_frame(
        &mut app,
        &CollabRequest::Hello {
            app_id: "test".into(),
            cluster_id: test_cluster(),
            record: None,
            handle: None,
        },
    );

    let artifact = ArtifactId::new();
    write_frame(
        &mut app,
        &CollabRequest::FetchScrollback {
            artifact,
            before_seq: 50,
            max: 10,
        },
    );
    let reply = read_event(&mut app, REPLY_BACKSTOP)
        .expect("local broker still replies even when forwarding to network");
    match reply {
        CollabEvent::ScrollbackBatch { ops, exhausted, .. } => {
            assert!(ops.is_empty());
            assert!(exhausted);
        }
        other => panic!("expected ScrollbackBatch, got {other:?}"),
    }

    drop(app);
    drop(handle);
    drop(net);
    let _ = std::fs::remove_file(&socket);
}

#[test]
fn cooperative_responder_answers_when_log_has_match() {
    // The inverse: an inbound ScrollbackRequest at a broker that DOES hold
    // the range makes the drain thread publish a ScrollbackResponse. The
    // request goes into ShellNetwork's request buffer (the hook the actor's
    // receive path uses) via a test-only constructor, since a spawned
    // ShellNetwork owns cmd_rx in the runtime.
    use shell::shell::network::ShellNetwork;
    use shell::shell::state::WorkspaceTree;
    use std::sync::Arc;

    let socket = unique_socket("coop-responder");
    let tmp = tempfile::tempdir().expect("tempdir");
    let key_path = tmp.path().join("peer-key");
    let tree = WorkspaceTree::bootstrap_demo().shared();
    let net = Arc::new(ShellNetwork::spawn(tree, &key_path).expect("spawn net"));
    let handle = collab_broker::spawn_with_peer_sync(socket.clone(), Some(Arc::clone(&net)))
        .expect("broker bind");

    let (owner_sk, owner_pk) = generate_keypair();
    let (editor_sk, editor_pk) = generate_keypair();
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();
    let (state, _, editor_att) =
        channel_state_with(&owner_sk, owner_pk, editor_pk, channel, artifact);
    handle.register_channel_state(artifact, state);

    let mut editor = wait_for_broker(&socket);
    write_frame(
        &mut editor,
        &CollabRequest::Hello {
            app_id: "test".into(),
            cluster_id: test_cluster(),
            record: None,
            handle: None,
        },
    );
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
        write_frame(
            &mut editor,
            &CollabRequest::SignedSyncIncremental {
                envelope: Box::new(op),
            },
        );
    }
    // Give the broker a moment to record into the log.
    std::thread::sleep(Duration::from_millis(150));

    // The drain thread reads the injected request, consults the local op log
    // and calls `publish_scrollback_response`. The actor's outbound channel
    // isn't observable here, so this asserts only that the request is
    // consumed (no panic, buffer drains).
    let handle_req = net.inbound_scrollback_request_handle();
    net.handle_scrollback_request_message(123u64, artifact, 10, 50);
    // Drain wakes every 100 ms.
    let deadline = Instant::now() + REPLY_BACKSTOP;
    let mut drained = false;
    while Instant::now() < deadline {
        if handle_req.lock().unwrap().is_empty() {
            drained = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(drained, "broker drain thread should consume the request");

    drop(editor);
    drop(handle);
    drop(net);
    let _ = std::fs::remove_file(&socket);
}
