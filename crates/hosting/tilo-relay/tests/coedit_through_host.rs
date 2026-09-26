//! Co-edit routed through the workspace host.
//!
//! The toolkit's co-edit deltas (`ui::collaboration`'s
//! `SyncIncremental` frames, carrying Automerge incrementals) are
//! bridged onto the host's `Envelope` transport by
//! [`tilo_relay::RelayCoeditSession`]. These tests prove that once
//! co-edit rides the host:
//!
//!   1. a live edit fans out to an online peer, and
//!   2. two peers that are **never simultaneously online** still
//!      converge on the shared document — the edit is retained on the
//!      host's disk once for the room and survives the host restarting.
//!
//! Convergence is asserted on genuine Automerge documents (identical
//! `get_heads()` + identical rendered text), on the real
//! `Relay`/`RelayCoeditSession` path — no mocks of the host or the
//! bridge.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::Duration;
use tilo_automerge::ApplyIncoming;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, Value};
use tilo_relay::{Inbound, RelayClient, RelayCoeditSession};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestUser, client_endpoint, spawn_relay_on};

/// A fresh document with a `Text` object at `ROOT["text"]` holding
/// `seed` — mirroring what a toolkit editor binds to.
fn doc_with_text(actor: &[u8], seed: &str) -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.set_actor(automerge::ActorId::from(actor));
    let t = doc.put_object(ROOT, "text", ObjType::Text).unwrap();
    doc.splice_text(&t, 0, 0, seed).unwrap();
    doc
}

fn text_of(doc: &AutoCommit) -> String {
    match doc.get(ROOT, "text") {
        Ok(Some((Value::Object(ObjType::Text), id))) => doc.text(&id).unwrap_or_default(),
        _ => String::new(),
    }
}

fn pending_count(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |rd| {
        rd.flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".delta"))
            .count()
    })
}

/// Drain `session` until an `Inbound::Delta` arrives, applying each
/// received delta to `doc`. Returns once a document delta has landed.
async fn recv_delta_into(session: &RelayCoeditSession, doc: &mut AutoCommit) {
    for _ in 0..8 {
        let inbound = tokio::time::timeout(Duration::from_secs(3), session.recv())
            .await
            .expect("recv timeout")
            .expect("session closed");
        match inbound {
            Inbound::Delta { data, .. } => {
                doc.load_incoming(&data).expect("apply co-edit delta");
                return;
            }
            // Ready / presence carry no document bytes — keep draining.
            Inbound::Ready { .. }
            | Inbound::Presence { .. }
            | Inbound::RemoteAction { .. }
            | Inbound::Replayed { .. } => {}
        }
    }
    panic!("no document delta arrived");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_coedit_fans_out_through_host() {
    const WS: u64 = 21;

    let user = TestUser::fresh("Alice");
    let a = user.enrol("laptop", DeviceCapabilities::primary());
    let b = user.enrol("phone", DeviceCapabilities::member());
    let members = [a.device_pubkey(), b.device_pubkey()];

    let state = tempfile::tempdir().unwrap();
    let host_dev = user.enrol("home-server", DeviceCapabilities::host());
    let host = spawn_relay_on(state.path(), &user, host_dev, &[WS], &members).await;
    let addr = host.node_addr();

    // Both peers connect and join the workspace's co-edit session.
    let a_client = RelayClient::connect(
        client_endpoint().await,
        addr.clone(),
        user.user_pk,
        a.cert.clone(),
        a.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("A connect");
    let a_session = RelayCoeditSession::join(a_client, WS, a.device_pubkey()).unwrap();

    let b_client = RelayClient::connect(
        client_endpoint().await,
        addr,
        user.user_pk,
        b.cert.clone(),
        b.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("B connect");
    let b_session = RelayCoeditSession::join(b_client, WS, b.device_pubkey()).unwrap();

    // Drain B's Ready(pending=0) so we know its subscription is live
    // before A publishes (live fan-out only reaches online subscribers).
    match tokio::time::timeout(Duration::from_secs(2), b_session.recv())
        .await
        .expect("B Ready timeout")
        .expect("B closed")
    {
        Inbound::Ready { pending } => assert_eq!(pending, 0),
        other => panic!("expected Ready, got {other:?}"),
    }

    // A produces a real co-edit delta and publishes it via the bridge.
    let mut doc_a = doc_with_text(b"alice", "hello");
    let delta = doc_a.save_incremental();
    a_session
        .publish_retained_delta([1; 32], *blake3::hash(&delta).as_bytes(), delta)
        .unwrap();

    // B converges by applying the fanned-out delta.
    let mut doc_b = AutoCommit::new();
    doc_b.set_actor(automerge::ActorId::from(b"bob".as_slice()));
    recv_delta_into(&b_session, &mut doc_b).await;

    assert_eq!(text_of(&doc_b), "hello", "B renders A's live co-edit");
    assert_eq!(
        doc_a.get_heads(),
        doc_b.get_heads(),
        "peers converge on the shared document through the host"
    );

    drop(host);
}

/// A connects to `addr1`, joins the co-edit session, drains its own
/// Ready(0), publishes `offline-note`, waits until the host has retained
/// it on disk for the (never-yet-connected) B, then disconnects.
async fn publish_offline_coedit(
    addr1: iroh_base::EndpointAddr,
    user: &TestUser,
    a: &crate::common::TestDevice,
    ws: u64,
    retained_dir: &std::path::Path,
) -> AutoCommit {
    let a_client = RelayClient::connect(
        client_endpoint().await,
        addr1,
        user.user_pk,
        a.cert.clone(),
        a.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("A connect");
    let a_session = RelayCoeditSession::join(a_client, ws, a.device_pubkey()).unwrap();
    // Drain A's own Ready(0).
    match tokio::time::timeout(Duration::from_secs(2), a_session.recv())
        .await
        .expect("A Ready timeout")
        .expect("A closed")
    {
        Inbound::Ready { pending } => assert_eq!(pending, 0),
        other => panic!("expected Ready, got {other:?}"),
    }

    let mut doc_a = doc_with_text(b"alice", "offline-note");
    let delta = doc_a.save_incremental();
    a_session
        .publish_retained_delta([2; 32], *blake3::hash(&delta).as_bytes(), delta)
        .unwrap();

    let mut queued = 0;
    for _ in 0..40 {
        queued = pending_count(retained_dir);
        if queued >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(queued, 1, "A's co-edit must be persisted for the offline B");

    // A disconnects; now neither device is online.
    drop(a_session);
    doc_a
}

/// B connects for the very first time (to the restarted host), joins,
/// and drains its offline co-edit queue through the bridge until both
/// the queued `Delta` and the `Ready` marker have arrived.
async fn catch_up_first_connection(
    addr2: iroh_base::EndpointAddr,
    user: &TestUser,
    b: &crate::common::TestDevice,
    ws: u64,
) -> AutoCommit {
    let b_client = RelayClient::connect(
        client_endpoint().await,
        addr2,
        user.user_pk,
        b.cert.clone(),
        b.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("B connect after restart");
    let b_session = RelayCoeditSession::join(b_client, ws, b.device_pubkey()).unwrap();

    // B drains its offline queue through the bridge and converges — even
    // though A and B were never online together and the host restarted.
    let mut doc_b = AutoCommit::new();
    doc_b.set_actor(automerge::ActorId::from(b"bob".as_slice()));
    let mut got_delta = false;
    let mut got_ready = false;
    for _ in 0..8 {
        if got_delta && got_ready {
            break;
        }
        let inbound = tokio::time::timeout(Duration::from_secs(3), b_session.recv())
            .await
            .expect("B catch-up timeout")
            .expect("B closed");
        match inbound {
            Inbound::Delta { data, .. } => {
                doc_b
                    .load_incoming(&data)
                    .expect("B applies queued co-edit");
                got_delta = true;
            }
            Inbound::Ready { pending } => {
                assert_eq!(pending, 1, "host reports the one queued co-edit");
                got_ready = true;
            }
            other @ (Inbound::Presence { .. }
            | Inbound::RemoteAction { .. }
            | Inbound::Replayed { .. }) => {
                panic!("unexpected inbound {other:?}")
            }
        }
    }
    assert!(got_delta, "B must receive the queued co-edit after restart");
    assert!(got_ready, "B must receive Ready");
    doc_b
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn never_both_online_coedit_converges_through_host() {
    const WS: u64 = 22;

    let user = TestUser::fresh("Alice");
    let a = user.enrol("laptop", DeviceCapabilities::primary());
    let b = user.enrol("phone", DeviceCapabilities::member());
    let members = [a.device_pubkey(), b.device_pubkey()];

    // The host's persistent state dir survives the restart below.
    let state = tempfile::tempdir().unwrap();
    let state_root = state.path().to_path_buf();

    // ---- Host boots; A joins, co-edits, and leaves. B has NEVER
    //      connected. ----
    let host1_dev = user.enrol("home-server", DeviceCapabilities::host());
    let host1 = spawn_relay_on(&state_root, &user, host1_dev, &[WS], &members).await;
    let addr1 = host1.node_addr();

    // B never connected. The co-edit is retained once for the room, never
    // copied into a per-device queue.
    let workspace_dir = state_root.join("workspaces").join(WS.to_string());
    let retained_dir = workspace_dir
        .join("retained/tails")
        .join(hex::encode([2u8; 32]));
    let pending_dir = workspace_dir
        .join("pending")
        .join(hex::encode(b.device_pubkey()));
    let mut doc_a = publish_offline_coedit(addr1, &user, &a, WS, &retained_dir).await;
    assert_eq!(pending_count(&pending_dir), 0, "no per-device copy for B");

    // ---- The host restarts on the same state directory. ----
    drop(host1);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        pending_count(&retained_dir),
        1,
        "the retained co-edit must survive the host process dying"
    );

    let host2_dev = user.enrol("home-server-2", DeviceCapabilities::host());
    let host2 = spawn_relay_on(&state_root, &user, host2_dev, &[WS], &members).await;
    let addr2 = host2.node_addr();

    // ---- B connects for the very first time, to the restarted host. ----
    let mut doc_b = catch_up_first_connection(addr2, &user, &b, WS).await;

    assert_eq!(text_of(&doc_b), "offline-note", "B renders A's co-edit");
    assert_eq!(
        doc_a.get_heads(),
        doc_b.get_heads(),
        "B converges with A across a host restart, never having been online together"
    );
    assert_eq!(
        pending_count(&retained_dir),
        1,
        "retained room state stays for the next member to join"
    );

    drop(host2);
}

/// Connect `device` to workspace `ws` on `addr` and wait for its `Ready`.
async fn join_ready(
    user: &TestUser,
    device: &crate::common::TestDevice,
    addr: iroh_base::EndpointAddr,
    ws: u64,
) -> RelayCoeditSession {
    let client = RelayClient::connect(
        client_endpoint().await,
        addr,
        user.user_pk,
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    let session = RelayCoeditSession::join(client, ws, device.device_pubkey()).unwrap();
    loop {
        let inbound = tokio::time::timeout(Duration::from_secs(5), session.recv())
            .await
            .unwrap()
            .unwrap();
        if matches!(inbound, Inbound::Ready { .. }) {
            return session;
        }
    }
}

/// A device asks for one document's retained state again — as it does when
/// an app attaches to that document after the device's own replay went by —
/// and gets exactly that channel's checkpoint and tail, in order, while
/// another channel's retained state stays out of it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_channel_replay_returns_exactly_that_channel() {
    const WS: u64 = 23;
    let user = TestUser::fresh("Alice");
    let a = user.enrol("laptop", DeviceCapabilities::primary());
    let b = user.enrol("phone", DeviceCapabilities::member());
    let members = [a.device_pubkey(), b.device_pubkey()];
    let state = tempfile::tempdir().unwrap();
    let host_dev = user.enrol("home-server", DeviceCapabilities::host());
    let host = spawn_relay_on(state.path(), &user, host_dev, &[WS], &members).await;
    let addr = host.node_addr();

    let a_session = join_ready(&user, &a, addr.clone(), WS).await;
    let (wanted, other) = ([0x31; 32], [0x32; 32]);
    let entry = |body: &[u8]| *blake3::hash(body).as_bytes();
    a_session
        .publish_checkpoint(
            wanted,
            entry(b"base"),
            tilo_relay::Coverage::Entries(Vec::new()),
            b"base".to_vec(),
        )
        .unwrap();
    for body in [b"one".as_slice(), b"two"] {
        a_session
            .publish_retained_delta(wanted, entry(body), body.to_vec())
            .unwrap();
    }
    a_session
        .publish_retained_delta(other, entry(b"elsewhere"), b"elsewhere".to_vec())
        .unwrap();

    let b_session = join_ready(&user, &b, addr, WS).await;
    let replayed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            b_session.request_channel_replay(wanted).unwrap();
            let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
            while let Ok(Some(inbound)) = tokio::time::timeout_at(deadline, b_session.recv()).await
            {
                if let Inbound::Replayed { channel, deltas } = inbound
                    && deltas.len() == 3
                {
                    return (channel, deltas);
                }
            }
        }
    })
    .await
    .expect("the channel is replayed once all three are retained");
    assert_eq!(replayed.0, wanted);
    let bodies: Vec<Vec<u8>> = replayed.1.into_iter().map(|(_, body)| body).collect();
    assert_eq!(
        bodies,
        vec![b"base".to_vec(), b"one".to_vec(), b"two".to_vec()]
    );
    drop(host);
}
