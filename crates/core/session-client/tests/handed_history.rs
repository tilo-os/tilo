//! A device that takes no part in its workspace's key group writes with the keys its host
//! handed it: the host's own actions, which act as the host device beside the host's own place
//! in the group. Its records' sealed histories fill with its writes like anyone's, and a full
//! history takes no write until it is compacted, so it compacts them with the same handed keys:
//! its writes, to the full record and to every other, reach the host, and a wait for them ends
//! once the compaction that carries the writes it folded is kept.

use automerge::transaction::Transactable as _;
use collab_types::DocumentMutation;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_session_client::{Cmd, Outbox as _};
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use crate::support::{
    WORKSPACE, commit_native, enrol, enter, entry_grant, route, start_host_plain, workspace,
};

/// More writes than a record's sealed history holds (4096) before it is compacted.
const CHANGES: usize = 4200;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_handed_its_keys_compacts_a_full_history_and_its_writes_go_out() {
    handed_keys_then_writes(Keys::First).await;
}

/// Writes made before the keys arrive wait for them, and then commit all at once: more of them
/// than a document's history holds still go out, as many at a time as it holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writes_held_for_the_keys_go_out_however_many_a_history_holds() {
    handed_keys_then_writes(Keys::AfterTheWrites).await;
}

/// When the server's keys reach the device: before it writes, or once it wrote everything.
#[derive(PartialEq, Eq)]
enum Keys {
    First,
    AfterTheWrites,
}

async fn handed_keys_then_writes(keys: Keys) {
    let (alice, _) = UserIdentity::generate("Alice", 1);
    let host_device = enrol(&alice, "alice-home", DeviceCapabilities::member());
    let (bob, _) = UserIdentity::generate("Bob", 1);
    let actions = enrol(&bob, "actions", DeviceCapabilities::member());
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
    // The keys the host hands: a group this device never joins.
    let group = key_group::GroupPeer::create_now(
        None,
        collab_types::WorkspaceId::parse(WORKSPACE).unwrap(),
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
        &actions.cert,
        &actions.key,
        &host_device.cert,
        workspace(),
    )
    .await;
    let tree = route();
    let tasks = DocumentRoute {
        document: "tasks".into(),
        ..route()
    };
    let install = || {
        for route in [&tree, &tasks] {
            peer.send(Cmd::InstallGroupKeys(
                route.clone(),
                group.content_keys().unwrap(),
            ))
            .unwrap();
        }
    };
    if keys == Keys::First {
        install();
    }

    // One record takes more than its sealed history holds, then another record is written.
    let mut doc = automerge::AutoCommit::new();
    doc.put(automerge::ROOT, "founded", true).unwrap();
    commit_native(&mut doc);
    peer.write(tree.clone(), &DocumentMutation::Snapshot(doc.save()))
        .unwrap();
    for change in 0..CHANGES {
        doc.put(automerge::ROOT, "count", change as u64).unwrap();
        commit_native(&mut doc);
        peer.write(
            tree.clone(),
            &DocumentMutation::Incremental(doc.save_incremental()),
        )
        .unwrap();
    }
    let mut other = automerge::AutoCommit::new();
    other.put(automerge::ROOT, "task", "write it down").unwrap();
    commit_native(&mut other);
    peer.write(tasks.clone(), &DocumentMutation::Snapshot(other.save()))
        .unwrap();

    if keys == Keys::AfterTheWrites {
        install();
    }

    // Every one of them is kept by the host: none waits on a history this device cannot compact.
    let receipt = peer.barrier().unwrap();
    let kept = tokio::time::timeout(tilo_test_env::hang_bound(), receipt)
        .await
        .ok();
    let status = peer.status();
    assert!(
        matches!(kept, Some(Ok(Ok(())))),
        "the writes were not all kept: {kept:?}; failures {:?}",
        status.failures
    );
    peer.close();
}
