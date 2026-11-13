//! A record's content is the workspace's members' own traffic: a host's ask for
//! what a record names reaches a linked member's engine, and the member's answer
//! reaches the host's, while the workspace's state takes neither in and nobody
//! refuses the other's frame.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::common::device;

use ed25519_dalek::SigningKey;
use neighbour_wire::{NeighbourMsg, peer_handle};
use std::sync::Arc;
use std::time::Duration;
use tilo_identity_host::custody::HostIdentity;
use tilo_peer_state::direct::DirectSync;
use tilo_peer_state::join_answer::JoinAnswer;
use tilo_peer_state::owner::testing;
use tilo_peer_state::owner::{NativeWorkspaceOwner, OwnerJoinWorkspaces, enroll, open_file};
use web_time::SystemTime;

/// This device's owner, in its own file under `state`.
fn owner_at(state: &std::path::Path) -> NativeWorkspaceOwner {
    open_file(&state.join("workspaces.json")).expect("this device's owner")
}

/// The invitation `host`'s device issues `member`'s person into the workspace it
/// founded, naming itself as the host to join through.
async fn invitation(
    host: &NativeWorkspaceOwner,
    workspace: tilo_desktop_model::WorkspaceId,
    host_cert: &tilo_user_identity::DeviceCert,
    host_key: &SigningKey,
    request_id: [u8; 32],
) -> String {
    let issued = host
        .issue_invitation(
            workspace,
            host_cert,
            &tilo_peer_state::join::invitations::IssueInvitationRequest {
                request_id,
                role: tilo_peer_state::join::MemberRole::Editor,
                recipient_user_public_key: None,
                invitee_name: None,
                expires_at_ns: tilo_clock::unix_now_ns().saturating_add(600_000_000_000),
                inviter_name: None,
            },
            &[testing::own_host_hint_here(workspace, host_cert, host_key)],
            host_key,
            SystemTime::now(),
        )
        .await
        .expect("the workspace's device issues an invitation");
    tilo_peer_state::join::encode_invitation_text(&issued.signed_invitation)
}

/// `member` redeems `invitation` into `workspace` through `host`'s own join
/// answers: what a device does with a link a person hands it.
async fn joins(
    member: &NativeWorkspaceOwner,
    certificate: &tilo_user_identity::DeviceCert,
    key: &SigningKey,
    workspace: tilo_desktop_model::WorkspaceId,
    invitation: String,
    host: &NativeWorkspaceOwner,
    host_key: &SigningKey,
) {
    let service: Arc<dyn tilo_peer_state::join_transport::LocalJoinService> = Arc::new(JoinAnswer(
        OwnerJoinWorkspaces::new(host.clone(), host_key.clone()),
    ));
    let identity = HostIdentity::kept(certificate.clone(), key.clone());
    let enrollment = enroll::prepare_and_complete(
        tilo_peer_state::join_transport::WorkspaceJoinClient::local(
            service,
            key.verifying_key().to_bytes(),
        ),
        identity,
        tilo_peer_state::join::WorkspaceId::parse(&workspace.to_string())
            .expect("a workspace topic"),
        None,
        invitation,
        member.clone(),
    )
    .await
    .expect("the workspace's device admits this one");
    member
        .install(&enrollment, SystemTime::now())
        .expect("the join installs here");
}

/// The next frame `heard` brings: a content frame, which the workspace's state
/// takes nothing of.
async fn next_content(
    heard: &mut tokio::sync::broadcast::Receiver<tilo_peer_state::direct::Heard>,
) -> tilo_peer_state::direct::Heard {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let frame = heard.recv().await.expect("the link stays open");
            if matches!(
                frame.message,
                NeighbourMsg::RecordContentWant { .. }
                    | NeighbourMsg::RecordContentPiece { .. }
                    | NeighbourMsg::RecordContentLacks { .. }
            ) {
                return frame;
            }
        }
    })
    .await
    .expect("the frame crosses the link")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hosts_ask_for_a_records_content_reaches_its_linked_member() {
    let root = SigningKey::from_bytes(&[41; 32]);
    let (host_key, host_cert) = device(&root, 42);
    let (member_key, member_cert) = device(&root, 43);
    let directory = tempfile::tempdir().unwrap();
    let host = owner_at(&directory.path().join("host"));
    let now = SystemTime::now();
    let workspace = testing::founded(&host, "Studio", &host_cert, &host_key, now)
        .await
        .expect("the workspace is founded on this device");
    let member = owner_at(&directory.path().join("member"));
    joins(
        &member,
        &member_cert,
        &member_key,
        workspace,
        invitation(&host, workspace, &host_cert, &host_key, [3; 32]).await,
        &host,
        &host_key,
    )
    .await;

    // The host serves the workspace; the member's device syncs it, both into
    // their own owners.
    let host_link = DirectSync::over(host.replica(workspace).unwrap(), host_key.clone())
        .owning(host.clone(), workspace)
        .unwrap();
    let member_link = DirectSync::over(member.replica(workspace).unwrap(), member_key.clone())
        .owning(member.clone(), workspace)
        .unwrap();
    // Subscribed before the link runs, so no frame it brings is missed.
    let mut at_host = host_link.heard();
    let mut at_member = member_link.heard();
    let mut caught_up = member_link.caught_up();
    let linking = tokio::spawn({
        let (host_link, member_link) = (host_link.clone(), member_link.clone());
        async move { DirectSync::pair(&member_link, &host_link).await }
    });
    // Both ends run before anything is said: what a link was handed before it
    // started is not the link's to carry.
    tokio::time::timeout(Duration::from_secs(30), caught_up.wait_for(|up| *up))
        .await
        .expect("the link catches up")
        .expect("the link stays open");
    // What the member's owner holds once the link is up: coming up is this
    // device's own work, its records' snapshots signed by it and the history
    // it takes in, and that commits. What follows rides the link as the
    // members' traffic and writes none of it.
    let generation = member
        .with_current(workspace, |view| Ok(view.generation()))
        .unwrap();

    // The host asks the workspace for a surface the changes it decides name.
    let ask = NeighbourMsg::RecordContentWant {
        requester: peer_handle(&host_cert.device_public_key),
        app_id: "so.tilo.chat".into(),
        cluster_id: workspace.to_string(),
        hash: "ab".repeat(32),
        have: Vec::new(),
    };
    host_link.send(ask.clone()).unwrap();
    let heard = next_content(&mut at_member).await;
    assert_eq!(heard.message, ask);
    assert_eq!(heard.author, host_cert.device_public_key);
    assert!(
        heard.report.is_ok(),
        "the member's engine takes the ask as content traffic, not a refusal: {:?}",
        heard.report
    );
    assert!(!heard.landed, "the workspace's state does not move with it");
    assert_eq!(
        member
            .with_current(workspace, |view| Ok(view.generation()))
            .unwrap(),
        generation,
        "the member's record is not written for a frame that is not its state"
    );

    // The member answers it over the same link, and the host hears the answer.
    let answer = NeighbourMsg::RecordContentLacks {
        responder: peer_handle(&member_cert.device_public_key),
        requester: peer_handle(&host_cert.device_public_key),
        app_id: "so.tilo.chat".into(),
        cluster_id: workspace.to_string(),
        hash: "ab".repeat(32),
    };
    member_link.send(answer.clone()).unwrap();
    let heard = next_content(&mut at_host).await;
    assert_eq!(heard.message, answer);
    assert_eq!(heard.author, member_cert.device_public_key);
    assert!(
        heard.report.is_ok(),
        "the host's engine takes the answer as content traffic, not a refusal: {:?}",
        heard.report
    );
    assert!(!heard.landed);
    linking.abort();
}
