//! Workspace grants are written only into a workspace this server's own person
//! governs: a server that keeps someone else's own workspace decides nothing
//! of it.

#![cfg(feature = "agents")]
#![allow(
    clippy::significant_drop_tightening,
    reason = "the test reads the server until its shutdown, which consumes it"
)]

use tilo_action_policy::{Answer, Grantee, KindSet, TreePath};

use super::OwnWorkspaceOffer;
use super::fixtures::{governor, test_host};

/// The person whose own workspace the server holds but does not govern.
const PERSON: [u8; 32] = [5; 32];
const BRANCH: &str = "6f2b8a51-6d0f-4d4e-9d6c-5d3b3f1f2a10";

#[tokio::test(flavor = "multi_thread")]
async fn grants_are_not_written_into_a_workspace_of_someone_elses() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let source = workspaces.create("Template").await.unwrap().workspace;
    let branch = workspaces
        .branch_own(&source, BRANCH, "Task", PERSON)
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

    // A workspace the server governs takes the grant...
    let governed = grant(&source);
    workspaces
        .put_workspace_grants(&source, &cert, [6; 32], vec![governed.clone()])
        .await
        .expect("the server's own workspace takes its grant");
    assert!(
        workspaces
            .workspace_grants(&source)
            .await
            .unwrap()
            .iter()
            .any(|kept| kept.id == governed.id)
    );

    // ...and one of someone else's own refuses it, writing nothing.
    let elsewhere = grant(&branch);
    assert!(
        workspaces
            .put_workspace_grants(&branch, &cert, [7; 32], vec![elsewhere.clone()])
            .await
            .is_err(),
        "the branch's person governs it, not this server's"
    );
    assert!(
        !workspaces
            .workspace_grants(&branch)
            .await
            .unwrap()
            .iter()
            .any(|kept| kept.id == elsewhere.id),
        "nothing was written into the branch"
    );
    server.shutdown().await;
}
