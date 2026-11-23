//! The one path a device's writes take: admitted into its own replica, kept as
//! a frame until a link carries it, and durable across a restart.

use automerge::{AutoCommit, ROOT, transaction::Transactable};
use collab_types::signer::DeviceSigner;
use ed25519_dalek::SigningKey;
use std::sync::Arc;
use tilo_peer_state::NeighbourMsg;
use tilo_peer_state::TransportOrigin;
use tilo_peer_state::owner::testing::{self, workspace::TestWorkspace};
use tilo_peer_state::owner::{LocalWrite, NativeWorkspaceOwner, open_file};

const APP: &str = "test.app";
const RECORD: &str = "tab:1";

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

/// A record holding `value`, as a whole snapshot.
fn note(value: &str) -> Vec<u8> {
    let mut document = AutoCommit::new();
    document.put(ROOT, "value", value).unwrap();
    document.commit();
    document.save()
}

/// Write `value` into the test record of `workspace` as `device`.
async fn write(
    owner: &NativeWorkspaceOwner,
    workspace: tilo_desktop_model::WorkspaceId,
    device: &SigningKey,
) {
    let written = owner
        .write_local(
            workspace,
            device.verifying_key().to_bytes(),
            Arc::new(device.clone()) as Arc<dyn DeviceSigner>,
            LocalWrite {
                app: APP,
                document: RECORD,
                readers: None,
                changes: &note("written"),
                founding: false,
                snapshot: true,
                sealed: None,
                now: tilo_clock::wall(),
            },
        )
        .await;
    written.expect("the device writes its record");
}

/// A workspace `root`'s person made, with the device `certificate` holding it,
/// and the fixture that holds it beside.
fn held(
    directory: &std::path::Path,
) -> (NativeWorkspaceOwner, TestWorkspace, SigningKey, SigningKey) {
    let root = key(1);
    let a_key = key(2);
    let b_key = key(3);
    let a_cert = testing::certificate(&root, &a_key, "A");
    let b_cert = testing::certificate(&root, &b_key, "B");
    let owner_a = open_file(&directory.join("a.json")).unwrap();
    let owner_b = open_file(&directory.join("b.json")).unwrap();
    let fixture = TestWorkspace::new();
    let fixture = testing::enroll_into(fixture, &owner_a, &a_key, &a_cert).unwrap();
    let fixture = testing::enroll_into(fixture, &owner_b, &b_key, &b_cert).unwrap();
    (owner_a, fixture, a_key, b_key)
}

/// A write is durable in this device's record before any peer holds it, and a
/// restart with the other device away keeps it until a link carries it there.
#[tokio::test]
async fn a_write_is_durable_before_a_peer_holds_it_and_survives_a_restart() {
    let directory = tempfile::tempdir().unwrap();
    let (owner_a, fixture, a_key, _b_key) = held(directory.path());
    let workspace = fixture.workspace_id();
    let owner_b = open_file(&directory.path().join("b.json")).unwrap();
    write(&owner_a, workspace, &a_key).await;
    let frames = owner_a.frames(workspace);
    assert!(!frames.is_empty(), "the write is a frame a link carries");
    assert!(
        owner_b.with_current(workspace, |view| Ok(view
            .peer()
            .holds_document(APP, RECORD)))
            == Ok(false),
        "the other device held nothing of it"
    );
    // The device goes away with the write only in its record, and comes back:
    // the frame is still there to be carried.
    drop(owner_a);
    let owner_a = open_file(&directory.path().join("a.json")).unwrap();
    assert_eq!(
        owner_a.frames(workspace),
        frames,
        "a restart keeps the frame until a peer holds it"
    );
    // A link carries it to the device that was away.
    let author = a_key.verifying_key().to_bytes();
    for frame in &frames {
        let message = NeighbourMsg::decode(frame).unwrap();
        owner_b
            .receive(
                workspace,
                TransportOrigin::ServerDelta(author),
                message,
                tilo_clock::wall(),
            )
            .unwrap();
        owner_a.sent(workspace, frame).unwrap();
    }
    assert!(
        owner_b.with_current(workspace, |view| Ok(view
            .peer()
            .holds_document(APP, RECORD)))
            == Ok(true),
        "the write reaches the device when it links"
    );
    assert!(
        owner_a.frames(workspace).is_empty(),
        "a frame a peer took is not carried again"
    );
}

/// The first hour with one device and no server: a workspace is made, written
/// to, a person is invited and joins, one device leaves, and the workspace
/// lives on the other.
#[tokio::test]
async fn one_device_holds_a_workspace_through_its_first_use_and_leaving_it() {
    let directory = tempfile::tempdir().unwrap();
    let (owner_a, fixture, a_key, b_key) = held(directory.path());
    let workspace = fixture.workspace_id();
    let owner_b = open_file(&directory.path().join("b.json")).unwrap();
    write(&owner_a, workspace, &a_key).await;
    assert!(
        owner_b
            .with_current(workspace, |view| Ok(view
                .peer()
                .holds_document(APP, RECORD)))
            .unwrap_or(false),
        "the joiner holds the workspace it was admitted into"
    );
    write(&owner_b, workspace, &b_key).await;
    assert!(
        owner_a.with_current(workspace, |view| Ok(view
            .peer()
            .holds_document(APP, RECORD)))
            == Ok(true)
    );
    assert!(owner_a.release_workspace(workspace).unwrap());
    assert!(!owner_a.contains_workspace(workspace));
    let reopened = open_file(&directory.path().join("a.json")).unwrap();
    assert!(
        !reopened.contains_workspace(workspace),
        "a workspace a device left does not come back"
    );
    assert!(
        owner_b.contains_workspace(workspace),
        "the workspace lives on the device that stayed"
    );
    let _ = fixture;
}
