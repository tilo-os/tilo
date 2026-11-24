//! A record is read only as the workspace it is in grants its caller: never on another
//! workspace's grants, and where the caller acts for someone, the smaller of the two grants.

#![cfg(feature = "agents")]

use tilo_action_policy::{Answer, Grant, Grantee, KindSet, TreePath};
use tilo_actions::remote::{ActionCallError, Caller, ResourceAddress, ResourceCopy, ResourceReads};

use super::super::OwnWorkspaceOffer;
use super::super::fixtures::{governor, test_host};

const NOTES: &str = "tilo.notes";
/// Someone no workspace here admitted.
const STRANGER: [u8; 32] = [9; 32];

/// `caller`'s read of a note in `workspace`, through the device's one reader.
async fn read(
    workspaces: &super::super::Workspaces,
    caller: Caller,
    workspace: &str,
) -> Result<ResourceCopy, ActionCallError> {
    let reads = workspaces.resource_reads();
    let address = ResourceAddress {
        workspace: workspace.to_owned(),
        app: NOTES.to_owned(),
        record: "groceries".to_owned(),
    };
    tokio::task::spawn_blocking(move || reads.read(&caller, &address))
        .await
        .expect("the read ran")
}

fn refused(read: &Result<ResourceCopy, ActionCallError>) -> bool {
    matches!(read, Err(ActionCallError::Refused { .. }))
}

/// A grant of `grantee` reading all of `workspace`, given by the person `by` names.
fn reads_all(id: &str, grantee: Grantee, workspace: &str, by: &str) -> Grant {
    Grant::new(
        id,
        grantee,
        TreePath::workspace(workspace),
        KindSet::READ_UNDOABLE,
        Answer::Allow,
        Grantee::person(by),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_foreign_workspace_grants_a_stranger_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let workspace = workspaces.create("Kitchen").await.unwrap().workspace;

    let stranger = Caller::authenticated(STRANGER, [8; 32]).in_workspace(workspace.clone());
    assert!(
        refused(&read(workspaces, stranger, &workspace).await),
        "a device the workspace never admitted reads nothing of it"
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_grant_in_one_workspace_reads_nothing_of_another() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let granting = workspaces.create("Kitchen").await.unwrap().workspace;
    let other = workspaces.create("Garden").await.unwrap().workspace;
    let person = hex::encode(cert.user_public_key);
    workspaces
        .put_workspace_grants(
            &granting,
            &cert,
            [6; 32],
            vec![reads_all(
                "bot:moderation",
                Grantee::bot("moderation"),
                &granting,
                &person,
            )],
        )
        .await
        .expect("the kitchen grants the bot its reads");

    // Run in the kitchen, the bot reads the garden on the garden's grants alone: it has none.
    let bot = Caller::local_bot("moderation").in_workspace(granting.clone());
    assert!(
        refused(&read(workspaces, bot, &other).await),
        "the kitchen's grant reads nothing of the garden"
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_app_acting_for_someone_reads_no_more_than_they_may() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let workspace = workspaces.create("Kitchen").await.unwrap().workspace;
    let person = hex::encode(cert.user_public_key);
    workspaces
        .put_workspace_grants(
            &workspace,
            &cert,
            [6; 32],
            vec![reads_all(
                "app:notes",
                Grantee::app(NOTES),
                &workspace,
                &person,
            )],
        )
        .await
        .expect("the workspace grants Notes its reads");

    // Notes may read the whole workspace, but the person it acts for here is no member: the
    // smaller grant, theirs, reads nothing.
    let notes = Caller::local_app(NOTES)
        .acting_for(hex::encode(STRANGER))
        .in_workspace(workspace.clone());
    assert!(
        refused(&read(workspaces, notes, &workspace).await),
        "an app reads no more than the person it acts for"
    );
    server.shutdown().await;
}
