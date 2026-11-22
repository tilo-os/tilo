//! A workspace's states reach whoever waits for them as events, through the real session and
//! host: a page that joins is told its workspace serves it, and a write it makes is told behind,
//! then done, once its host kept it. Nothing here waits on a timer.

use collab_types::DocumentMutation;
use tilo_session_client::{Cmd, Event, Outbox as _, SyncState, WorkspaceState};
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use crate::support::{WORKSPACE, enrol, enter, entry_grant, route, start_host_plain, workspace};

/// The next state event on `events` that `wanted` takes, waited for as the session wakes it.
async fn told<T>(
    peer: &tilo_session_client::Link,
    events: &tilo_session_client::Events,
    mut wanted: impl FnMut(&Event) -> Option<T>,
) -> T {
    let mut seen = Vec::new();
    let found = peer
        .until(tilo_test_env::hang_bound(), |_| {
            for event in events.drain() {
                if let Some(found) = wanted(&event) {
                    return Some(found);
                }
                seen.push(format!("{event:?}"));
            }
            None
        })
        .await;
    found.unwrap_or_else(|| panic!("the state never came; told instead: {seen:?}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_joined_workspace_and_a_write_are_told_by_their_states() {
    let (alice, _) = UserIdentity::generate("Alice", 1);
    let host_device = enrol(&alice, "alice-home", DeviceCapabilities::member());
    let (bob, _) = UserIdentity::generate("Bob", 1);
    let browser = enrol(&bob, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host_plain(
        &alice,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
        None,
    )
    .await;
    let board = host
        .server
        .workspaces()
        .unwrap()
        .create_workspace("Board", Some(WORKSPACE))
        .await
        .unwrap();
    let route = route();
    let creator = key_group::GroupPeer::create_now(
        None,
        collab_types::WorkspaceId::parse(&route.cluster_id).unwrap(),
        Box::new(bob.clone()),
        &key_group::VaultKey::new([7; 32]),
    )
    .unwrap();
    let peer = enter(
        &host,
        entry_grant(
            &host,
            &alice,
            &host_device,
            &board.workspace,
            bob.verifying_key().to_bytes(),
        )
        .await,
        &browser.cert,
        &browser.key,
        &host_device.cert,
        workspace(),
    )
    .await;
    let events =
        peer.events_where(|event| matches!(event, Event::Workspace { .. } | Event::Sync(_)));

    // The page is in: its workspace is open on the session, and nothing it wrote is owed.
    let status = peer.status();
    assert!(
        matches!(
            status.workspace_state(workspace()),
            Some(WorkspaceState::Open | WorkspaceState::Serving)
        ),
        "{:?}",
        status.workspace_state(workspace())
    );
    assert_eq!(status.sync_state(), SyncState::Done);

    // A write is owed to the host until it kept it: behind, then done, each told once.
    peer.send(Cmd::InstallGroupKeys(
        route.clone(),
        creator.content_keys().unwrap(),
    ))
    .unwrap();
    let mut doc = automerge::AutoCommit::new();
    automerge::transaction::Transactable::put(&mut doc, automerge::ROOT, "note", "hello").unwrap();
    peer.write(route.clone(), &DocumentMutation::Snapshot(doc.save()))
        .unwrap();
    let behind = told(&peer, &events, |event| match event {
        Event::Sync(SyncState::Behind { pending }) => Some(*pending),
        _ => None,
    })
    .await;
    assert!(behind > 0);
    told(&peer, &events, |event| {
        matches!(event, Event::Sync(SyncState::Done)).then_some(())
    })
    .await;
    assert_eq!(peer.status().sync_state(), SyncState::Done);

    // The host goes away: the workspace is told it waits for a connection.
    host.server.shutdown().await;
    let state = told(&peer, &events, |event| match event {
        Event::Workspace { ws, state } if *ws == workspace() => Some(*state),
        _ => None,
    })
    .await;
    assert_eq!(state, WorkspaceState::Connecting);
    peer.close();
}
