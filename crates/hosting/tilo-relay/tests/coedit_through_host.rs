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
//!      converge on the shared document — the edit is store-and-
//!      forwarded on disk and survives the host process restarting.
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

fn retained_count(state_root: &std::path::Path, workspace: u64) -> usize {
    tilo_relay::store::WorkspaceStore::open(state_root, workspace, false)
        .unwrap()
        .load_retained()
        .unwrap()
        .len()
}

/// Drain `session` until an `Inbound::Delta` arrives, applying each
/// received delta to `doc`. Returns once a document delta has landed.
async fn recv_delta_into(session: &RelayCoeditSession, doc: &mut AutoCommit) {
    for _ in 0..8 {
        let inbound = tilo_test_env::within("recv timeout", session.recv())
            .await
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
            | Inbound::Live { .. } => {}
        }
    }
    panic!("no document delta arrived");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
async fn coedits_converge_through_the_host_live_and_offline() {
    // Phase `live_coedit_fans_out_through_host`.
    {
        const WS: u64 = 21;

        let user = TestUser::fresh("Alice");
        let a = user.enrol("laptop", DeviceCapabilities::primary());
        let b = user.enrol("phone", DeviceCapabilities::member());
        let members = [a.device_pubkey(), b.device_pubkey()];

        let state = tempfile::tempdir().unwrap();
        let host_dev = user.enrol("home-server", DeviceCapabilities::host());
        let host = Box::pin(spawn_relay_on(
            state.path(),
            &user,
            host_dev,
            &[WS],
            &members,
        ))
        .await;
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
        match tilo_test_env::within("B Ready timeout", b_session.recv())
            .await
            .expect("B closed")
        {
            Inbound::Ready { pending } => assert_eq!(pending, 0),
            other => panic!("expected Ready, got {other:?}"),
        }

        // A produces a real co-edit delta and publishes it via the bridge.
        let mut doc_a = doc_with_text(b"alice", "hello");
        let delta = doc_a.save_incremental();
        a_session.publish_retained_delta([1; 32], delta).unwrap();

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
    // Phase `never_both_online_coedit_converges_through_host`.
    {
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
        let host1 = Box::pin(spawn_relay_on(
            &state_root,
            &user,
            host1_dev,
            &[WS],
            &members,
        ))
        .await;
        let addr1 = host1.node_addr();

        // Shared durable coverage serves B even though it has never connected.
        let mut doc_a = publish_offline_coedit(addr1, &user, &a, WS, &state_root).await;

        // ---- The host restarts on the same state directory. ----
        drop(host1);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            retained_count(&state_root, WS),
            1,
            "the queued co-edit must survive the host process dying"
        );

        let host2_dev = user.enrol("home-server-2", DeviceCapabilities::host());
        let host2 = Box::pin(spawn_relay_on(
            &state_root,
            &user,
            host2_dev,
            &[WS],
            &members,
        ))
        .await;
        let addr2 = host2.node_addr();

        // ---- B connects for the very first time, to the restarted host. ----
        let mut doc_b = catch_up_first_connection(addr2, &user, &b, WS).await;

        assert_eq!(text_of(&doc_b), "offline-note", "B renders A's co-edit");
        assert_eq!(
            doc_a.get_heads(),
            doc_b.get_heads(),
            "B converges with A across a host restart, never having been online together"
        );
        let store = tilo_relay::store::WorkspaceStore::open(&state_root, WS, false).unwrap();
        assert_eq!(
            store.count_pending(&b.device_pubkey()).unwrap(),
            0,
            "retained delivery must not create per-device copies"
        );
        assert_eq!(
            store.load_retained().unwrap().len(),
            1,
            "the same coverage remains available to newly invited devices"
        );

        drop(host2);
    }
}

/// A connects to `addr1`, joins the co-edit session, drains its own
/// Ready(0), publishes `offline-note`, waits until the host has queued
/// it on disk for the (never-yet-connected) B, then disconnects.
async fn publish_offline_coedit(
    addr1: iroh_base::EndpointAddr,
    user: &TestUser,
    a: &crate::common::TestDevice,
    ws: u64,
    state_root: &std::path::Path,
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
    match tilo_test_env::within("A Ready timeout", a_session.recv())
        .await
        .expect("A closed")
    {
        Inbound::Ready { pending } => assert_eq!(pending, 0),
        other => panic!("expected Ready, got {other:?}"),
    }

    let mut doc_a = doc_with_text(b"alice", "offline-note");
    let delta = doc_a.save_incremental();
    a_session.publish_retained_delta([2; 32], delta).unwrap();

    let mut queued = 0;
    // Polls for up to 30 s; only a hang uses the whole budget.
    for _ in 0..1200 {
        queued = retained_count(state_root, ws);
        if queued >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        queued, 1,
        "A's co-edit must be durably retained for the offline B"
    );

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
        let inbound = tilo_test_env::within("B catch-up timeout", b_session.recv())
            .await
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
            | Inbound::Live { .. }) => {
                panic!("unexpected inbound {other:?}")
            }
        }
    }
    assert!(got_delta, "B must receive the queued co-edit after restart");
    assert!(got_ready, "B must receive Ready");
    doc_b
}

/// A session measures how long the host has been silent, and a ping is
/// answered: what a device's watchdog uses to tell a host that stopped
/// serving its session from one that is merely idle.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pinged_host_answers_and_resets_the_silence() {
    const WS: u64 = 23;

    let user = TestUser::fresh("Alice");
    let a = user.enrol("laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let host_dev = user.enrol("home-server", DeviceCapabilities::host());
    let host = spawn_relay_on(state.path(), &user, host_dev, &[WS], &[a.device_pubkey()]).await;
    let client = RelayClient::connect(
        client_endpoint().await,
        host.node_addr(),
        user.user_pk,
        a.cert.clone(),
        a.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("connect");
    let session = RelayCoeditSession::join(client, WS, a.device_pubkey()).unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), session.recv())
            .await
            .expect("Ready timeout"),
        Some(Inbound::Ready { .. })
    ));

    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(session.silent_for() >= Duration::from_secs(1));
    session.ping().unwrap();
    // A Pong carries no co-edit event, so `recv` keeps waiting after it.
    let _ = tokio::time::timeout(Duration::from_millis(200), session.recv()).await;
    assert!(
        session.silent_for() < Duration::from_millis(500),
        "the Pong counts as hearing from the host"
    );
    // The host serves until here.
    drop(host);
}
