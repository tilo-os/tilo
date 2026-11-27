//! Workspace grants are written into a workspace this server's own person
//! administers: how far a server acts in one is its own row on the member list.

#![cfg(feature = "agents")]
#![allow(
    clippy::significant_drop_tightening,
    reason = "the test reads the server until its shutdown, which consumes it"
)]

use tilo_action_policy::{Answer, Grantee, TreePath};
use tilo_action_types::KindSet;

use super::OwnWorkspaceOffer;
use super::fixtures::{hosting_device, test_host};

/// The person whose own workspace the server holds.
const PERSON: [u8; 32] = [5; 32];
const BRANCH: &str = "6f2b8a51-6d0f-4d4e-9d6c-5d3b3f1f2a10";

#[tokio::test(flavor = "multi_thread")]
async fn grants_are_written_into_a_workspace_as_far_as_this_servers_person_administers_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let source = workspaces.create("Template").await.unwrap().workspace;
    let branch = Box::pin(workspaces.branch_own(&source, BRANCH, "Task", PERSON))
        .await
        .map_err(|not| not.refusal())
        .unwrap()
        .workspace;
    let grant = |workspace: &str| {
        tilo_action_policy::Grant::new(
            "bot:moderation",
            Grantee::bot("moderation"),
            TreePath::workspace(workspace),
            KindSet::READ_UNDOABLE,
            Answer::Allow,
            Grantee::person(hex::encode(cert.user_public_key)),
        )
    };

    // A workspace the server hosts takes the grant...
    let hosted = grant(&source);
    workspaces
        .put_workspace_grants(&source, &cert, [6; 32], vec![hosted.clone()])
        .await
        .expect("the server's own workspace takes its grant");
    assert!(
        workspaces
            .workspace_grants(&source)
            .await
            .unwrap()
            .iter()
            .any(|kept| kept.id == hosted.id)
    );

    // ...and one the server made for someone else takes it too: the server's person stays in
    // that workspace as one of its administrators, and that row is what lets the server act
    // there. A list that gives its person no such place carries out nothing for the member
    // asking (see `authority::tests::a_server_without_a_place_on_the_member_list_carries_out_nothing`).
    let elsewhere = grant(&branch);
    workspaces
        .put_workspace_grants(&branch, &cert, [7; 32], vec![elsewhere.clone()])
        .await
        .expect("the branch's administrator takes its grant");
    assert!(
        workspaces
            .workspace_grants(&branch)
            .await
            .unwrap()
            .iter()
            .any(|kept| kept.id == elsewhere.id),
        "the grant the branch's administrator wrote is kept"
    );
    server.shutdown().await;
}
