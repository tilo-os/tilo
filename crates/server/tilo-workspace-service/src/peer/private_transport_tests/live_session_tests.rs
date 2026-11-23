//! The live session: a device that joins is told who is on it, and every
//! device already there is told about it. Ephemeral traffic, never kept.

use super::*;

/// The live members one connection heard about, in the order they reached it.
fn heard(rx: &mut mpsc::Receiver<Envelope>) -> Vec<([u8; 32], bool)> {
    let mut members = Vec::new();
    while let Ok(envelope) = rx.try_recv() {
        if let Envelope::LiveMember {
            device_public_key,
            active,
            ..
        } = envelope
        {
            members.push((device_public_key, active));
        }
    }
    members
}

/// A device joining a workspace's live session learns who is already on it,
/// and each of them learns it joined; a device that leaves is told to the
/// others as gone.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_joining_the_live_session_is_told_to_the_others() {
    let (root, user) = UserIdentity::generate("alice", 1);
    let (a, _) = certified_device(&root, user.public_key, "a");
    let (b, _) = certified_device(&root, user.public_key, "b");
    let id = WorkspaceId::from_u128(12);
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = crate::workspace::WorkspaceRuntime::open(tmp.path(), id).unwrap();
    runtime.list_environment(a.device_public_key, true);
    runtime.list_environment(b.device_public_key, true);
    let (atx, mut arx) = mpsc::channel(8);
    let (adisconnect, _adisconnect_rx) = mpsc::channel(1);
    let (btx, mut brx) = mpsc::channel(8);
    let (bdisconnect, _bdisconnect_rx) = mpsc::channel(1);
    for (peer, tx, disconnect) in [(&a, &atx, &adisconnect), (&b, &btx, &bdisconnect)] {
        runtime.add_subscriber(Subscriber {
            device_public_key: peer.device_public_key,
            tx: tx.clone(),
            disconnect: disconnect.clone(),
            content_waiting: Arc::default(),
            peer: peer.clone(),
        });
    }
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(
        id,
        Arc::new(AsyncMutex::new(runtime)),
    )])));
    let holds = Holds::open();

    let ctx_a = holds.ctx(&registry, &atx, &adisconnect, (&user.public_key, &a.cert));
    assert!(
        handle_live_join(&a, &ctx_a, &[id], id)
            .await
            .unwrap()
            .is_none(),
        "the first device joins"
    );

    let ctx_b = holds.ctx(&registry, &btx, &bdisconnect, (&user.public_key, &b.cert));
    assert!(
        handle_live_join(&b, &ctx_b, &[id], id)
            .await
            .unwrap()
            .is_none(),
        "the second device joins"
    );
    assert_eq!(
        heard(&mut arx),
        vec![(b.device_public_key, true)],
        "the device already there is told the second joined"
    );
    assert_eq!(
        heard(&mut brx),
        vec![(a.device_public_key, true)],
        "the second is told who was already there"
    );

    let workspace = registry.lock().await.get(&id).cloned().unwrap();
    workspace.lock().await.live_leave(&b.device_public_key, id);
    assert_eq!(
        heard(&mut arx),
        vec![(b.device_public_key, false)],
        "the device still there is told the other left"
    );
}
