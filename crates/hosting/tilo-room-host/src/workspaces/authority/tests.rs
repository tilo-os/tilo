//! A workspace this relay governs decides its own administration: what another
//! person's device may do is what the workspace's governance lets that person,
//! and what no workspace decides stays the relay's person's.

#![expect(
    clippy::significant_drop_tightening,
    reason = "the relay and workspace handles a test binds are shared handles it keeps until the test ends; dropping one earlier closes the host it drives and releases no lock"
)]

use ed25519_dalek::Signer as _;
use tilo_action_policy::{Answer, Grant, KindSet, Subject, roles, roles::governance};
use tilo_peer_state::join::MemberRole;
use tilo_relay_protocol::AuthenticatedPeer;
use tilo_relay_protocol::voice::{CallOperation, CallState};
use tilo_relay_protocol::workspaces::{
    JoinReply, JoinStep, WorkspacesReply, WorkspacesRequest, join_proof_digest,
};
use tilo_user_identity::{DeviceCert, sign_device_cert};

use crate::workspaces::tests::{governor, guest_device, signed_pre_admission, test_host};
use crate::workspaces::{
    CancelInvitationRequest, IssueInvitationRequest, PreAdmitRequest, RemoveMemberRequest,
    SetMemberRoleRequest, Workspaces,
};

/// A new person admitted to `workspace` as an Editor, through the owner's
/// pre-admission: their device's certificate.
async fn admit(
    workspaces: &Workspaces,
    workspace: &str,
    owner: &DeviceCert,
    seed: u8,
) -> DeviceCert {
    let token = [seed; 8];
    let issued = workspaces
        .pre_admit(
            owner,
            PreAdmitRequest {
                request_id: [seed; 32],
                grant: signed_pre_admission(workspace, &token),
                recipient_user_public_key: None,
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let (key, cert) = guest_device();
    let JoinReply::Challenge { challenge } = workspaces
        .page_join(JoinStep::Ask {
            invitation: issued.signed_invitation,
            device_cert: cert.clone(),
            request_id: rand::random(),
        })
        .await
    else {
        panic!("no challenge");
    };
    let signature = key.sign(&join_proof_digest(&challenge)).to_bytes();
    let reply = workspaces
        .page_join(JoinStep::ProvePreAdmitted {
            challenge,
            signature,
            name: format!("Person {seed}"),
            token: token.to_vec(),
        })
        .await;
    assert!(matches!(reply, JoinReply::Admitted { .. }), "{reply:?}");
    cert
}

async fn grant(
    workspaces: &Workspaces,
    workspace: &str,
    owner: &DeviceCert,
    person: &DeviceCert,
    action: &str,
) {
    let who = hex::encode(person.user_public_key);
    let grant = Grant::new(
        format!("test:{action}:{who}"),
        Subject::Person(who),
        roles::governance_action(workspace, action),
        KindSet::ALL,
        Answer::Allow,
        Subject::Person(hex::encode(owner.user_public_key)),
    );
    let request_id = *blake3::hash(grant.id.as_str().as_bytes()).as_bytes();
    workspaces
        .put_room_grants(workspace, owner, request_id, vec![grant])
        .await
        .unwrap();
}

async fn set_role(
    workspaces: &Workspaces,
    workspace: &str,
    by: &DeviceCert,
    person: &DeviceCert,
    role: MemberRole,
) {
    workspaces
        .set_member_role(workspace, by, role_request(person, role, rand::random()))
        .await
        .unwrap();
}

fn role_request(
    person: &DeviceCert,
    role: MemberRole,
    request_id: [u8; 32],
) -> SetMemberRoleRequest {
    SetMemberRoleRequest {
        request_id,
        user_public_key: person.user_public_key,
        role,
    }
}

fn invitation(request_id: u8, role: MemberRole) -> IssueInvitationRequest {
    IssueInvitationRequest {
        request_id: [request_id; 32],
        role,
        recipient_user_public_key: None,
        invitee_name: None,
        expires_at_ns: tilo_clock::unix_now_ns().saturating_add(600_000_000_000),
        inviter_name: None,
    }
}

#[tokio::test]
async fn a_member_granted_invite_runs_invitations_and_an_editor_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let ada = admit(workspaces, &workspace, &owner, 1).await;

    // An Editor by role alone governs nothing.
    assert!(
        workspaces
            .issue(&workspace, &ada, invitation(10, MemberRole::Viewer))
            .await
            .is_err()
    );
    assert!(workspaces.pending(&workspace, &ada).await.is_err());
    assert!(matches!(
        workspaces
            .page_rooms(
                &ada,
                WorkspacesRequest::Pending {
                    workspace: workspace.clone()
                }
            )
            .await,
        WorkspacesReply::Refused(_)
    ));

    // The workspace grants Ada its invite action: she invites, up to her role.
    grant(workspaces, &workspace, &owner, &ada, governance::INVITE).await;
    let issued = workspaces
        .issue(&workspace, &ada, invitation(11, MemberRole::Editor))
        .await
        .unwrap();
    assert!(
        workspaces
            .pending(&workspace, &ada)
            .await
            .unwrap()
            .iter()
            .any(|pending| pending.invite_id == issued.invite_id)
    );
    let above = workspaces
        .issue(&workspace, &ada, invitation(12, MemberRole::Moderator))
        .await
        .unwrap_err();
    assert!(above.to_string().contains("Moderator"), "{above}");
    assert!(matches!(
        workspaces
            .page_rooms(
                &ada,
                WorkspacesRequest::Pending {
                    workspace: workspace.clone()
                }
            )
            .await,
        WorkspacesReply::Pending(_)
    ));
    assert!(matches!(
        workspaces
            .page_rooms(
                &ada,
                WorkspacesRequest::Notices {
                    workspace: workspace.clone()
                }
            )
            .await,
        WorkspacesReply::Notices(_)
    ));
    workspaces
        .cancel(
            &workspace,
            &ada,
            CancelInvitationRequest {
                request_id: [13; 32],
                invite_id: issued.invite_id,
            },
        )
        .await
        .unwrap();
    assert!(
        !workspaces
            .pending(&workspace, &owner)
            .await
            .unwrap()
            .iter()
            .any(|pending| pending.invite_id == issued.invite_id)
    );
    relay.shutdown().await;
}

#[tokio::test]
async fn roles_and_removals_follow_the_rooms_policy() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let cy = admit(workspaces, &workspace, &owner, 3).await;
    let dee = admit(workspaces, &workspace, &owner, 4).await;

    // A Moderator removes an Editor, but sets no roles.
    set_role(workspaces, &workspace, &owner, &bo, MemberRole::Moderator).await;
    assert!(
        workspaces
            .set_member_role(
                &workspace,
                &bo,
                role_request(&cy, MemberRole::Viewer, [20; 32])
            )
            .await
            .is_err()
    );
    workspaces
        .remove_member(
            &workspace,
            &bo,
            RemoveMemberRequest {
                request_id: [21; 32],
                user_public_key: dee.user_public_key,
            },
        )
        .await
        .unwrap();

    // Granted set-role, Ada hands out roles up to her own, and never
    // changes an administrator's: only the owner makes or unmakes those.
    assert!(
        workspaces
            .set_member_role(
                &workspace,
                &ada,
                role_request(&cy, MemberRole::Viewer, [22; 32])
            )
            .await
            .is_err()
    );
    grant(workspaces, &workspace, &owner, &ada, governance::SET_ROLE).await;
    set_role(workspaces, &workspace, &ada, &cy, MemberRole::Viewer).await;
    assert!(
        workspaces
            .set_member_role(
                &workspace,
                &ada,
                role_request(&cy, MemberRole::Moderator, [23; 32])
            )
            .await
            .is_err()
    );
    workspaces
        .set_member_administrator(&workspace, &owner, [24; 32], cy.user_public_key)
        .await
        .unwrap();
    let administrator = workspaces
        .set_member_role(
            &workspace,
            &ada,
            role_request(&cy, MemberRole::Viewer, [25; 32]),
        )
        .await
        .unwrap_err();
    assert!(
        administrator.to_string().contains("administrators"),
        "{administrator}"
    );
    // The owner's devices still change anyone's role.
    set_role(workspaces, &workspace, &owner, &cy, MemberRole::Editor).await;

    // Nobody removes the owner, the owner's devices included.
    for by in [&owner, &bo] {
        assert!(
            workspaces
                .remove_member(
                    &workspace,
                    by,
                    RemoveMemberRequest {
                        request_id: rand::random(),
                        user_public_key: owner.user_public_key,
                    },
                )
                .await
                .is_err()
        );
    }
    relay.shutdown().await;
}

#[tokio::test]
async fn an_administrator_governs_within_their_role_without_further_grants() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let cy = admit(workspaces, &workspace, &owner, 3).await;
    set_role(workspaces, &workspace, &owner, &cy, MemberRole::Viewer).await;

    workspaces
        .set_member_administrator(&workspace, &owner, [50; 32], ada.user_public_key)
        .await
        .unwrap();
    an_administrator_invites_within_their_role(workspaces, &workspace, &ada).await;
    an_administrator_rules_those_ranked_below(workspaces, &workspace, &ada, &bo, &cy).await;
    an_administrator_runs_the_workspaces_calls(workspaces, &workspace, &ada).await;
    unmaking_the_administrator_takes_it_all_back(workspaces, &workspace, &owner, &ada, &bo).await;
    relay.shutdown().await;
}

/// The co-owner invites, up to their own role, with no grant beyond the owner
/// making them an administrator, and reads the requests asked of the
/// workspace.
async fn an_administrator_invites_within_their_role(
    workspaces: &Workspaces,
    workspace: &str,
    ada: &DeviceCert,
) {
    let issued = workspaces
        .issue(workspace, ada, invitation(51, MemberRole::Editor))
        .await
        .unwrap();
    assert!(
        workspaces
            .pending(workspace, ada)
            .await
            .unwrap()
            .iter()
            .any(|pending| pending.invite_id == issued.invite_id)
    );
    let above = workspaces
        .issue(workspace, ada, invitation(52, MemberRole::Moderator))
        .await
        .unwrap_err();
    assert!(above.to_string().contains("Moderator"), "{above}");
    assert!(matches!(
        workspaces
            .page_rooms(
                ada,
                WorkspacesRequest::Pending {
                    workspace: workspace.to_owned()
                }
            )
            .await,
        WorkspacesReply::Pending(_)
    ));
}

/// They set roles up to their own and remove those ranked below them, never a
/// fellow Editor.
async fn an_administrator_rules_those_ranked_below(
    workspaces: &Workspaces,
    workspace: &str,
    ada: &DeviceCert,
    bo: &DeviceCert,
    cy: &DeviceCert,
) {
    set_role(workspaces, workspace, ada, cy, MemberRole::Commenter).await;
    assert!(
        workspaces
            .set_member_role(
                workspace,
                ada,
                role_request(bo, MemberRole::Moderator, [53; 32])
            )
            .await
            .is_err()
    );
    assert!(
        workspaces
            .remove_member(
                workspace,
                ada,
                RemoveMemberRequest {
                    request_id: [54; 32],
                    user_public_key: bo.user_public_key,
                },
            )
            .await
            .is_err()
    );
    workspaces
        .remove_member(
            workspace,
            ada,
            RemoveMemberRequest {
                request_id: [55; 32],
                user_public_key: cy.user_public_key,
            },
        )
        .await
        .unwrap();
}

/// They run the workspace's calls.
async fn an_administrator_runs_the_workspaces_calls(
    workspaces: &Workspaces,
    workspace: &str,
    ada: &DeviceCert,
) {
    let end = CallOperation::End {
        call: [9; 32],
        epoch: 1,
    };
    let answer = workspaces.voice_call(workspace, &peer(ada), &end).await;
    assert!(
        answer
            .as_ref()
            .err()
            .is_some_and(|reason| reason.contains("unknown call")),
        "{answer:?}"
    );
}

/// An Editor by role alone still governs nothing, and the owner unmaking the
/// administrator takes it all back.
async fn unmaking_the_administrator_takes_it_all_back(
    workspaces: &Workspaces,
    workspace: &str,
    owner: &DeviceCert,
    ada: &DeviceCert,
    bo: &DeviceCert,
) {
    assert!(
        workspaces
            .issue(workspace, bo, invitation(56, MemberRole::Viewer))
            .await
            .is_err()
    );
    set_role(workspaces, workspace, owner, ada, MemberRole::Editor).await;
    assert!(
        workspaces
            .issue(workspace, ada, invitation(57, MemberRole::Viewer))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn the_next_governance_change_rewrites_an_administrators_old_grant() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let who = hex::encode(ada.user_public_key);
    // An administrator's grant as workspaces held it before: on the workspace's
    // resource, where the Editor's own Deny outranked it.
    let legacy = Grant::new(
        roles::administrator_grant_id(&who).as_str(),
        Subject::Person(who),
        roles::workspace_resource(&workspace),
        KindSet::ALL,
        Answer::Allow,
        Subject::Person(hex::encode(owner.user_public_key)),
    );
    workspaces
        .put_room_grants(&workspace, &owner, [60; 32], vec![legacy])
        .await
        .unwrap();
    workspaces
        .issue(&workspace, &ada, invitation(61, MemberRole::Editor))
        .await
        .unwrap();
    relay.shutdown().await;
}

#[tokio::test]
async fn what_no_room_decides_stays_the_relay_persons() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    // Ada holds every governance action the workspace has.
    for action in governance::ALL {
        grant(workspaces, &workspace, &owner, &ada, action).await;
    }
    workspaces
        .issue(&workspace, &ada, invitation(30, MemberRole::Viewer))
        .await
        .unwrap();

    assert!(
        workspaces.enter(&workspace, &ada).await.is_err(),
        "entering makes an owner"
    );
    for request in [
        WorkspacesRequest::List,
        WorkspacesRequest::Create {
            name: "Another".into(),
        },
        WorkspacesRequest::Enter {
            workspace: workspace.clone(),
        },
    ] {
        let reply = workspaces.page_rooms(&ada, request.clone()).await;
        assert!(
            matches!(reply, WorkspacesReply::Refused(_)),
            "{request:?}: {reply:?}"
        );
    }
    assert!(
        workspaces
            .set_member_administrator(&workspace, &ada, [31; 32], bo.user_public_key)
            .await
            .is_err()
    );
    assert!(
        workspaces
            .put_room_grants(&workspace, &ada, [32; 32], Vec::new())
            .await
            .is_err()
    );
    assert!(
        workspaces
            .pre_admit(
                &ada,
                PreAdmitRequest {
                    request_id: [33; 32],
                    grant: signed_pre_admission(&workspace, b"token"),
                    recipient_user_public_key: None,
                    inviter_name: None,
                },
            )
            .await
            .is_err()
    );
    assert_eq!(workspaces.list().await.len(), 1);
    relay.shutdown().await;
}

#[tokio::test]
async fn calls_are_run_by_the_owner_and_by_members_granted_call() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    set_role(workspaces, &workspace, &owner, &bo, MemberRole::Moderator).await;
    // Ending a call the workspace never had: past the gate, the call itself
    // answers that it is unknown.
    let end = CallOperation::End {
        call: [9; 32],
        epoch: 1,
    };
    let past_the_gate = |answer: &Result<CallState, String>| {
        answer
            .as_ref()
            .err()
            .is_some_and(|reason| reason.contains("unknown call"))
    };
    let owners = workspaces.voice_call(&workspace, &peer(&owner), &end).await;
    assert!(past_the_gate(&owners), "{owners:?}");
    for member in [&ada, &bo] {
        let answer = workspaces.voice_call(&workspace, &peer(member), &end).await;
        assert!(answer.is_err() && !past_the_gate(&answer), "{answer:?}");
    }
    grant(workspaces, &workspace, &owner, &ada, governance::CALL).await;
    let granted = workspaces.voice_call(&workspace, &peer(&ada), &end).await;
    assert!(past_the_gate(&granted), "{granted:?}");
    relay.shutdown().await;
}

#[tokio::test]
async fn an_expired_certificate_administers_nothing_even_the_owners() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let mut expired = owner.clone();
    expired.expires_at_ns = Some(2);
    sign_device_cert(
        &mut expired,
        &ed25519_dalek::SigningKey::from_bytes(&[1; 32]),
    )
    .unwrap();
    assert!(
        workspaces
            .issue(&workspace, &expired, invitation(40, MemberRole::Viewer))
            .await
            .is_err()
    );
    assert!(workspaces.pending(&workspace, &expired).await.is_err());
    assert!(workspaces.enter(&workspace, &expired).await.is_err());
    workspaces.pending(&workspace, &owner).await.unwrap();
    relay.shutdown().await;
}

fn peer(cert: &DeviceCert) -> AuthenticatedPeer {
    AuthenticatedPeer {
        cert: cert.clone(),
        user_public_key: cert.user_public_key,
        device_public_key: cert.device_public_key,
    }
}
