#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
#![allow(
    clippy::too_many_lines,
    reason = "each test spells out one end-to-end scenario"
)]
use super::private_transport_tests::certified_device;
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use tilo_user_identity::{RevocationList, UserIdentity};

/// A delivery its fan-out authorised under the workspace's lock is written
/// without taking that lock again while nothing the fan-out read has
/// changed, under the lock once anything announced a change, and not at all
/// once its lease ended.
#[tokio::test]
async fn a_fanned_delivery_skips_the_rooms_lock_until_authority_moves() {
    let temp = tempfile::tempdir().unwrap();
    let id = 7;
    let mut runtime = crate::workspace::WorkspaceRuntime::open(
        temp.path(),
        id,
        true,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    let (root, user) = UserIdentity::generate("fan-out fixture", 1);
    let (recipient, _) = certified_device(&root, user.public_key, "recipient");
    let (outbound, _outbound_rx) = mpsc::channel(8);
    let (disconnect, _disconnected) = mpsc::channel(1);
    let (state_tx, _state_rx) = mpsc::channel(8);
    let live = Arc::new(AtomicBool::new(true));
    runtime.admit_device(recipient.device_public_key).unwrap();
    runtime.add_subscriber(Subscriber {
        device_public_key: recipient.device_public_key,
        tx: outbound.clone(),
        disconnect: disconnect.clone(),
    });
    runtime.state_sessions.insert(
        recipient.device_public_key,
        StateSession {
            peer: recipient.clone(),
            tx: state_tx.clone(),
            live: live.clone(),
            replay: None,
            lag: None,
        },
    );
    let workspace = Arc::new(AsyncMutex::new(runtime));
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(id, workspace.clone())])));
    // Only a workspace whose membership a live watch vouches for is stamped.
    let watch = crate::membership_watch::spawn_membership_watch(temp.path());
    tilo_test_env::within("the membership watch to vouch for the room", async {
        while !crate::workspace_io::fresh_membership(&workspace)
            .await
            .unwrap()
            .membership_is_current()
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    let policy = Arc::new(std::sync::RwLock::new(
        crate::policy::TrustPolicy::AllowList {
            users: vec![hex::encode(user.public_key)],
        },
    ));
    let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let session = SessionCtx {
        workspaces: &registry,
        autobind: None,
        outbound: &outbound,
        state_outbound: Some(&state_tx),
        connection_live: None,
        own_user_public_key: &user.public_key,
        own_device_cert: &recipient.cert,
        own_device_public_key: &recipient.cert.device_public_key,
        app_serving: None,
        metrics: &metrics,
        trust_policy: &policy,
        disconnect: &disconnect,
        presence: None,
        revocations: &revocations,
        carried_calls: None,
        admin: None,
        here: None,
        agents: Box::leak(Box::default()),
        positions: None,
        compactor: None,
        member_limits: None,
        resumes: Box::leak(Box::default()),
        lag: Box::leak(Box::default()),
    };
    let delivery = |fanned: Option<FanoutStamp>| StateDelivery {
        envelope: Arc::new(Envelope::Delta {
            workspace_id: id,
            author_device_public_key: [9; 32],
            authored_at_ns: 1,
            delta: vec![1, 2, 3],
            signature: [0; 64],
        }),
        cleanup: None,
        source: None,
        _budget: None,
        live: live.clone(),
        fanned,
    };
    let write = |delivery: StateDelivery| {
        let session = &session;
        let recipient = &recipient;
        async move {
            let mut wire = Vec::new();
            let result = write_state_delivery(
                delivery,
                recipient,
                session,
                &mut SessionCoding::default(),
                &crate::presence_mailbox::PresenceMailbox::default(),
                &mut wire,
            )
            .await;
            (result, wire)
        }
    };

    // The workspace stays locked, as a crowd's other turns keep it.
    let held = workspace.lock().await;
    // The epoch is process-wide: another test may move it between the stamp
    // and the write, which only sends this one the locked way. One of a few
    // fresh stamps reaches the wire while the workspace is held.
    let mut wire = Vec::new();
    for _ in 0..20 {
        let stamp = FanoutStamp::of(&held);
        assert!(stamp.is_some(), "a watched room's fan-out is stamped");
        if let Ok((result, written)) = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            write(delivery(stamp)),
        )
        .await
        {
            result.unwrap();
            wire = written;
            break;
        }
    }
    assert!(
        !wire.is_empty(),
        "an unchanged fan-out must be written without the room's lock"
    );

    // Anything announced since the stamp sends the writer under the lock.
    let stale = FanoutStamp::of(&held);
    crate::authority_epoch::bump();
    let mut waiting = Box::pin(write(delivery(stale)));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(300), &mut waiting)
            .await
            .is_err(),
        "a stamp older than an announced change must wait for the room's lock"
    );
    drop(held);
    let (result, written) = waiting.await;
    result.unwrap();
    assert!(
        !written.is_empty(),
        "the recipient is still a member, so the locked check writes it"
    );

    // An ended lease is never written, stamped or not.
    live.store(false, Ordering::SeqCst);
    let stamp = FanoutStamp::of(&*workspace.lock().await);
    let (result, written) = write(delivery(stamp)).await;
    result.unwrap();
    assert!(written.is_empty(), "an ended lease must not reach the wire");
    watch.abort();
}
