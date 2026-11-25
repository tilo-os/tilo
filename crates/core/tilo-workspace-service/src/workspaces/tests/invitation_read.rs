//! What a person waiting to join sees first: the card that reads their
//! invitation answers at once, however busy the workspace it admits into is.

use std::time::{Duration, Instant};

use tilo_peer_state::join::Role;
use tilo_server_protocol::workspaces::{JoinReply, WorkspacesReply, WorkspacesRequest};
use tilo_user_identity::DeviceCert;

use crate::workspaces::tests::{hosting_device, test_host};
use crate::workspaces::{WaitListSecrets, Workspaces};

/// What reading an invitation may take: one local check of what it signs, plus
/// the cheap lookup of whether its workspace still honours it.
const BUDGET: Duration = Duration::from_millis(300);

/// The invitation the owner of `workspace` made for `bo@example.org`, which
/// left its address at the workspace's entry: what a person new here is mailed
/// and reads before joining.
async fn mailed_invitation(
    workspaces: &Workspaces,
    owner: &DeviceCert,
    workspace: &str,
) -> Vec<u8> {
    let secrets = WaitListSecrets {
        removal: [0x0b; 32],
        confirm: Some([0x1b; 32]),
    };
    workspaces
        .wait(workspace, "bo@example.org", secrets, "en", 10)
        .await
        .unwrap();
    assert!(
        workspaces
            .confirm_wait_list(workspace, [0x1b; 32])
            .await
            .unwrap()
    );
    let WorkspacesReply::WaitList(listed) = workspaces
        .page_workspaces(
            owner,
            WorkspacesRequest::WaitList {
                workspace: workspace.to_owned(),
            },
        )
        .await
    else {
        panic!("the owner reads the wait list");
    };
    let entry = listed
        .iter()
        .find(|entry| entry.contact == "bo@example.org")
        .unwrap_or_else(|| panic!("bo's address is listed: {listed:?}"))
        .request_id;
    let WorkspacesReply::Invitation {
        signed_invitation, ..
    } = workspaces
        .page_workspaces(
            owner,
            WorkspacesRequest::Invite {
                workspace: workspace.to_owned(),
                request_id: [42; 32],
                role: Role::Editor,
                expires_at_ns: tilo_clock::unix_now_ns().saturating_add(600_000_000_000),
                inviter_name: None,
                wait_list_entry: Some(entry),
                recipient: None,
                administrator: false,
            },
        )
        .await
    else {
        panic!("the owner invites the address");
    };
    signed_invitation
}

/// The invitation is read for the card the person answers before joining: it
/// must not wait on the workspace, whose lock is held across what takes long
/// (asking the key service, a commit's writes) while the workspace is busy.
#[tokio::test]
async fn an_invitation_reads_at_once_though_its_workspace_is_busy() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let workspace = workspaces.create("Lounge").await.unwrap().workspace;
    let invitation = mailed_invitation(workspaces, &owner, &workspace).await;

    // The workspace just made, as the first person to open its invitation
    // finds it.
    let began = Instant::now();
    let reply = workspaces.inspect_invitation(&invitation).await;
    assert!(
        matches!(reply, JoinReply::Invitation { .. }),
        "the server honours its own invitation: {reply:?}"
    );
    let fresh = began.elapsed();
    assert!(fresh < BUDGET, "a fresh read took {fresh:?}");

    // The workspace busy with what takes long, as one opening its files or
    // committing a member's writes is: its lock is held throughout.
    let held = workspaces.workspace(&workspace).await.unwrap();
    let taken = std::sync::Arc::new(tokio::sync::Notify::new());
    let signalling = std::sync::Arc::clone(&taken);
    let holding = tokio::spawn(async move {
        let _holding = held.lock().await;
        signalling.notify_one();
        tokio::time::sleep(Duration::from_secs(2)).await;
    });
    taken.notified().await;

    let began = Instant::now();
    let busy_reply = workspaces.inspect_invitation(&invitation).await;
    let busy = began.elapsed();
    holding.abort();
    assert!(
        matches!(busy_reply, JoinReply::Invitation { .. }),
        "the invitation still reads while the workspace is busy: {busy_reply:?}"
    );
    assert!(busy < BUDGET, "a read of a busy workspace took {busy:?}");

    server.shutdown().await;
}
