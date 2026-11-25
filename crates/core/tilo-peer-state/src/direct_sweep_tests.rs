//! A host's store may hold changes to its workspace's own state that the
//! workspace refused: an earlier build kept each change a device sent it
//! before the workspace decided it, and served it to every device that linked
//! after, where it was refused again. A host drops them when it opens the
//! workspace, judging each as it judges what a link brings.

use super::*;
use crate::owner::testing::{WORKSPACE, hosted_fixture_at, member_key};
use crate::store::{Appetite, Store};
use ed25519_dalek::SigningKey;
use std::sync::Arc;
use tilo_desktop_model::WorkspaceId;

/// A device no workspace lists: a change kept under it is refused for good.
const OUTSIDER: [u8; 32] = [9; 32];

fn workspace() -> WorkspaceId {
    WorkspaceId::parse(WORKSPACE).unwrap()
}

/// The host as a restart serves it: its store read again from `root`, over
/// the governor's replica, before any device is served it.
fn reopened(root: &std::path::Path, governor: &StatePeer, key: &SigningKey) -> DirectSync {
    let store = Arc::new(Store::open(root, Appetite::StoreAll).unwrap());
    let link = DirectSync::over(Replica::new(governor.clone()), key.clone()).with_store(store);
    link.host();
    link
}

/// A host keeps a refused change to its workspace's own state the way a build
/// that judged nothing did: it drops the change when it opens the workspace,
/// serves the change the workspace took in, and a device that links after
/// takes all of it in without waiting for history.
#[tokio::test]
async fn a_change_the_workspace_refuses_is_dropped_when_the_host_opens_it() {
    let ((_, _, _, _, now), mut governor, key) = hosted_fixture_at(tilo_clock::wall());
    let workspace = workspace();
    // One change the workspace takes in — its governor renames it — and one
    // it refuses: the same rename, kept under a device its roster does not
    // list, as a frame a host judged after keeping it.
    let mut authoring = governor.clone();
    authoring
        .governance_mut()
        .author_workspace_name("Refused", &key, now)
        .unwrap();
    let refused = authoring.workspace_message();
    governor
        .governance_mut()
        .author_workspace_name("Kept", &key, now)
        .unwrap();
    let accepted = governor.workspace_message();
    assert_ne!(accepted, refused, "the two changes are other frames");

    let root = tempfile::tempdir().unwrap();
    let governor_device = key.verifying_key().to_bytes();
    {
        let store = Store::open(root.path(), Appetite::StoreAll).unwrap();
        store
            .keep_workspace_change(workspace, OUTSIDER, &refused)
            .unwrap();
        store
            .keep_workspace_change(workspace, governor_device, &accepted)
            .unwrap();
    }

    let host = reopened(root.path(), &governor, &key);
    assert_eq!(
        host.drop_refused_workspace_changes().await,
        1,
        "the change the workspace refuses is dropped, and the one it takes in stays"
    );

    // What a device that links now is sent: the change the workspace took in.
    let served = host.link_catch_up(true).unwrap();
    assert!(
        served.iter().any(|authored| authored.message == accepted),
        "the change the workspace took in is served"
    );
    assert!(
        !served.iter().any(|authored| authored.message == refused),
        "the change it refused is served to nobody"
    );

    // And a device that links takes all of it in without waiting for history.
    let roster = governor.certificates();
    let host_cert = roster
        .iter()
        .find(|cert| cert.device_public_key == governor_device)
        .unwrap()
        .clone();
    let member = DirectSync::new(
        StatePeer::from_join(WORKSPACE, host_cert, roster, &[], now).unwrap(),
        member_key(),
    );
    let history = served
        .into_iter()
        .map(|authored| (authored.from, authored.message))
        .collect();
    assert!(
        !member.admit_history(history).await,
        "nothing left waits for history it was not sent"
    );
    assert_eq!(
        member.read(|peer| peer.governance().workspace_name()),
        Some("Kept".into()),
        "the member holds the change the workspace took in"
    );
    assert_eq!(
        host.store()
            .unwrap()
            .kept_workspace_changes(workspace)
            .len(),
        1
    );
}

/// A change the workspace takes in but this device cannot apply yet waits
/// for history: it is no refusal, and the sweep keeps it for the device that
/// links after.
#[tokio::test]
async fn a_change_that_waits_for_history_is_kept() {
    let ((_, _, _, _, now), governor, key) = hosted_fixture_at(tilo_clock::wall());
    let workspace = workspace();
    let governor_device = key.verifying_key().to_bytes();
    // A change made, never sent, and one built on it: only the second is
    // kept, so whoever takes it in waits for the first.
    let mut writer = governor.clone();
    writer
        .governance_mut()
        .author_workspace_name("Made", &key, now)
        .unwrap();
    let parent = writer.workspace_heads().unwrap();
    writer
        .governance_mut()
        .author_workspace_name("Sent", &key, now)
        .unwrap();
    let changes = writer.workspace_changes_since(&parent);
    assert!(!changes.is_empty(), "the change built on it is sent");
    let waiting = NeighbourMsg::SyncResp {
        changes,
        workspace: WORKSPACE.into(),
    };

    let root = tempfile::tempdir().unwrap();
    {
        let store = Store::open(root.path(), Appetite::StoreAll).unwrap();
        store
            .keep_workspace_change(workspace, governor_device, &waiting)
            .unwrap();
    }
    let host = reopened(root.path(), &governor, &key);
    assert_eq!(
        host.drop_refused_workspace_changes().await,
        0,
        "a change that waits for history is no change the workspace refuses"
    );
    assert_eq!(
        host.store().unwrap().kept_workspace_changes(workspace),
        vec![(governor_device, waiting)],
        "it is kept for the device that links after"
    );
}
