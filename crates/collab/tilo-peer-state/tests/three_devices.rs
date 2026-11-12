//! A workspace has members, not one governing device: a participant that does not
//! host the workspace answers a join and removes a member, every device admits
//! what it wrote, and the removed device is refused everywhere.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use std::time::SystemTime;
use tilo_delivery_protocol::workspace_join::{IrohWorkspaceHostHintV1, WorkspaceHostMode};
use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::TransportOrigin;
use tilo_peer_state::join::invitations::IssueInvitationRequest;
use tilo_peer_state::join::{
    self, JoinChallengeRequest, JoinCompleteRequest, JoinTransportRequest, JoinTransportResponse,
    MemberRole, TransportHint,
};
use tilo_peer_state::join_answer::JoinAnswer;
use tilo_peer_state::owner::testing::{self, WORKSPACE, governor::TestGovernor};
use tilo_peer_state::owner::{
    NativeWorkspaceOwner, OwnerJoinWorkspaces, VerifiedNativeEnrollmentIntent, open_file,
};
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_peer_state::workspace_membership::{SettingsChange, WorkspaceMembershipCommand};
use tilo_user_identity::{DeviceCert, RevocationList};

/// A device at `key` under `root`.
fn device(root: u8, key: u8, label: &str) -> (SigningKey, SigningKey, DeviceCert) {
    let root = SigningKey::from_bytes(&[root; 32]);
    let key = SigningKey::from_bytes(&[key; 32]);
    let cert = testing::certificate(&root, &key, label);
    (root, key, cert)
}

/// The host's signed hint that it answers the workspace's joins at its own endpoint.
fn hint(cert: &DeviceCert, key: &SigningKey) -> TransportHint {
    IrohWorkspaceHostHintV1 {
        version: 1,
        mode: WorkspaceHostMode::NativeInteractiveWorkspaceOwner,
        room: WORKSPACE.into(),
        workspace_id: join::room_workspace_id(WORKSPACE).unwrap(),
        endpoint_addr: iroh_base::EndpointAddr::new(
            iroh_base::EndpointId::from_bytes(&cert.device_public_key).unwrap(),
        ),
        host_user_public_key: cert.user_public_key,
        host_device_certificate: cert.clone(),
        packet_relay_url: "iroh:direct".into(),
    }
    .transport_hint(key)
    .unwrap()
}

/// `governor`'s answer to `request` from `remote` at `now`. Every step of an
/// engine's answer is decided before its future is first polled.
fn answer(
    governor: &JoinAnswer<OwnerJoinWorkspaces>,
    remote: [u8; 32],
    request: JoinTransportRequest,
    now: SystemTime,
) -> JoinTransportResponse {
    let answer = std::pin::pin!(governor.answer(remote, request, testing::ns(now).unwrap()));
    match answer.poll(&mut std::task::Context::from_waker(std::task::Waker::noop())) {
        std::task::Poll::Ready(answer) => answer,
        std::task::Poll::Pending => panic!("an engine answers at once"),
    }
}

/// Enroll `owner` as `certificate`'s device into the workspace `governor` holds.
fn enroll(
    owner: &NativeWorkspaceOwner,
    key: &SigningKey,
    certificate: &DeviceCert,
    governor: &mut TestGovernor,
    role: MemberRole,
    request_id: [u8; 32],
    now: SystemTime,
) {
    let now_ns = testing::ns(now).unwrap();
    let invitation = governor.issue(
        vec![governor.hint(iroh_base::EndpointAddr::new(
            iroh_base::EndpointId::from_bytes(&certificate.device_public_key).unwrap(),
        ))],
        role,
        Some(certificate.user_public_key),
        None,
        now_ns.checked_add(60_000_000_000).unwrap(),
    );
    let verified = join::decode_and_verify_invitation(
        &invitation,
        Some(certificate.user_public_key),
        &RevocationList::new(),
        now_ns,
    )
    .unwrap();
    let hint = verified
        .grant()
        .transport
        .first()
        .cloned()
        .expect("the invitation appoints the host's hint");
    let JoinTransportResponse::Challenge(challenge) = governor.handle(
        certificate.device_public_key,
        JoinTransportRequest::Challenge(JoinChallengeRequest {
            signed_invitation: invitation,
            device_cert: certificate.clone(),
            request_id,
        }),
    ) else {
        panic!("the workspace's host challenges the device");
    };
    let complete = JoinCompleteRequest {
        proof: join::sign_join_proof(challenge.challenge, key).unwrap(),
    };
    let intent = VerifiedNativeEnrollmentIntent::from_proof(
        verified,
        complete.clone(),
        certificate.clone(),
        hint,
        now,
    )
    .unwrap();
    let pending = owner.prepare_enrollment_into(&intent, None, now).unwrap();
    let JoinTransportResponse::Complete(bootstrap) = governor.handle(
        certificate.device_public_key,
        JoinTransportRequest::Complete(complete),
    ) else {
        panic!("the workspace's host admits the device");
    };
    owner
        .install(&testing::finalize(intent, pending, bootstrap).unwrap(), now)
        .unwrap();
}

/// What `owner` publishes for its workspace now.
fn outgoing(
    owner: &NativeWorkspaceOwner,
    workspace: WorkspaceId,
    now: SystemTime,
) -> Vec<NeighbourMsg> {
    owner
        .with_outgoing(workspace, now, |_, outgoing| {
            Ok(outgoing
                .entries()
                .map(|entry| postcard::from_bytes(entry.bytes()).unwrap())
                .collect())
        })
        .unwrap()
}

/// Whether `owner`'s workspace still holds `certificate` as a member device.
fn is_member(
    owner: &NativeWorkspaceOwner,
    workspace: WorkspaceId,
    certificate: &DeviceCert,
    now: SystemTime,
) -> bool {
    owner
        .with_current(workspace, |view| {
            Ok(view
                .peer()
                .permissions(certificate.device_public_key, now)
                .is_ok())
        })
        .unwrap()
}

/// A workspace with three people: its owner's device answers the workspace's host, a
/// second person's device holds it as a participant, and a third joins
/// through that participant, which then removes them.
#[tokio::test]
async fn a_participant_that_does_not_host_the_workspace_admits_and_removes_members() {
    let directory = tempfile::tempdir().unwrap();
    let (_root_a, key_a, cert_a) = device(91, 92, "A's desktop");
    let (_root_b, key_b, cert_b) = device(73, 74, "B's desktop");
    let (_root_c, key_c, cert_c) = device(75, 76, "C's desktop");
    let workspace = WorkspaceId::parse(WORKSPACE).unwrap();

    // A's device is the workspace's host: it made the workspace.
    let mut governor =
        TestGovernor::empty_interactive(cert_a.clone(), key_a.clone(), WORKSPACE).unwrap();
    let now = SystemTime::now();
    governor.pin_clock(now);
    let a = open_file(&directory.path().join("a.json")).unwrap();
    enroll(
        &a,
        &key_a,
        &cert_a,
        &mut governor,
        MemberRole::Moderator,
        [1; 32],
        now,
    );
    assert!(
        a.with_current(workspace, |view| Ok(view.hosted_here()))
            .unwrap(),
        "A's device holds the workspace it made"
    );

    // The workspace's owner lets B invite: what a participant admits with.
    let invite = tilo_action_policy::Grant::new(
        "invite:b",
        tilo_action_policy::Subject::person(hex::encode(cert_b.user_public_key)),
        tilo_action_policy::roles::governance_action(
            WORKSPACE,
            tilo_action_policy::roles::governance::INVITE,
        ),
        tilo_action_policy::KindSet::of(tilo_action_policy::roles::GOVERNANCE_KIND),
        tilo_action_policy::Answer::Allow,
        tilo_action_policy::Subject::person(hex::encode(cert_a.user_public_key)),
    );
    a.commit_workspace_membership_command(
        OwnerCommitRequest {
            workspace: WORKSPACE.into(),
            operation: CommitOperation::WorkspaceMembershipCommand {
                invocation: uuid::Uuid::new_v4(),
                caller: cert_a.clone(),
                command: WorkspaceMembershipCommand::Grant { grant: invite },
            },
        },
        &key_a,
        now,
    )
    .await
    .unwrap();
    for message in outgoing(&a, workspace, now) {
        governor
            .receive(
                TransportOrigin::RetainedRelayDelta(cert_a.device_public_key),
                message,
            )
            .unwrap();
    }

    // B is a member, a participant of the workspace on their own device.
    let b = open_file(&directory.path().join("b.json")).unwrap();
    enroll(
        &b,
        &key_b,
        &cert_b,
        &mut governor,
        MemberRole::Moderator,
        [2; 32],
        now,
    );
    assert!(
        !b.with_current(workspace, |view| Ok(view.hosted_here()))
            .unwrap(),
        "B's device holds the workspace without hosting it"
    );
    // A's device takes the workspace as its host has it now, B's membership
    // included.
    for (bytes, _) in governor.outgoing() {
        a.receive(
            workspace,
            TransportOrigin::RetainedRelayDelta(cert_a.device_public_key),
            postcard::from_bytes(bytes).unwrap(),
            now,
        )
        .unwrap();
    }
    assert!(
        is_member(&a, workspace, &cert_b, now),
        "A admits B's membership"
    );

    // C joins through B's own engine: the invitation B issues and the
    // admission B commits are B's, published like any change B makes.
    let c = open_file(&directory.path().join("c.json")).unwrap();
    let issued = b
        .issue_invitation(
            workspace,
            &cert_b,
            &IssueInvitationRequest {
                request_id: [3; 32],
                role: MemberRole::Editor,
                recipient_user_public_key: Some(cert_c.user_public_key),
                invitee_name: None,
                expires_at_ns: testing::ns(now)
                    .unwrap()
                    .checked_add(60_000_000_000)
                    .unwrap(),
                inviter_name: None,
            },
            &[hint(&cert_b, &key_b)],
            &key_b,
            now,
        )
        .await
        .unwrap();
    let joined = JoinAnswer(OwnerJoinWorkspaces::new(b.clone(), key_b.clone()));
    let verified = join::decode_and_verify_invitation(
        &issued.signed_invitation,
        Some(cert_c.user_public_key),
        &RevocationList::new(),
        testing::ns(now).unwrap(),
    )
    .unwrap();
    let JoinTransportResponse::Challenge(challenge) = answer(
        &joined,
        cert_c.device_public_key,
        JoinTransportRequest::Challenge(JoinChallengeRequest {
            signed_invitation: issued.signed_invitation,
            device_cert: cert_c.clone(),
            request_id: [4; 32],
        }),
        now,
    ) else {
        panic!("B's engine challenges C");
    };
    let complete = JoinCompleteRequest {
        proof: join::sign_join_proof(challenge.challenge, &key_c).unwrap(),
    };
    let intent = VerifiedNativeEnrollmentIntent::from_proof(
        verified,
        complete.clone(),
        cert_c.clone(),
        hint(&cert_b, &key_b),
        now,
    )
    .unwrap();
    let pending = c.prepare_enrollment_into(&intent, None, now).unwrap();
    let JoinTransportResponse::Complete(bootstrap) = answer(
        &joined,
        cert_c.device_public_key,
        JoinTransportRequest::Complete(complete),
        now,
    ) else {
        panic!("B's engine admits C");
    };
    c.install(&testing::finalize(intent, pending, bootstrap).unwrap(), now)
        .unwrap();
    assert!(
        is_member(&b, workspace, &cert_c, now),
        "B's engine admitted C"
    );
    for message in outgoing(&b, workspace, now) {
        a.receive(
            workspace,
            TransportOrigin::RelayDelta(cert_b.device_public_key),
            message,
            now,
        )
        .unwrap();
    }
    assert!(
        is_member(&a, workspace, &cert_c, now),
        "A admits what B admitted"
    );
    // B removes C: no governing device signs it, the workspace's policy does.
    b.commit_workspace_membership_command(
        OwnerCommitRequest {
            workspace: WORKSPACE.into(),
            operation: CommitOperation::WorkspaceMembershipCommand {
                invocation: uuid::Uuid::new_v4(),
                caller: cert_b.clone(),
                command: WorkspaceMembershipCommand::Member {
                    change: tilo_action_policy::Change::RemoveMember {
                        person: hex::encode(cert_c.user_public_key),
                    },
                },
            },
        },
        &key_b,
        now,
    )
    .await
    .unwrap();
    assert!(
        !is_member(&b, workspace, &cert_c, now),
        "B's own engine dropped C"
    );
    for (label, owner) in [("A", &a), ("C", &c)] {
        for message in outgoing(&b, workspace, now) {
            owner
                .receive(
                    workspace,
                    TransportOrigin::RelayDelta(cert_b.device_public_key),
                    message.clone(),
                    now,
                )
                .unwrap_or_else(|error| panic!("{label} refuses B's removal: {error}"));
        }
    }
    assert!(
        !is_member(&a, workspace, &cert_c, now),
        "A converged on the removal B made"
    );
    // The removed device holds the same workspace and may write nothing into it.
    assert!(
        !is_member(&c, workspace, &cert_c, now),
        "C holds its own removal"
    );
    assert!(
        c.commit_workspace_membership_command(
            OwnerCommitRequest {
                workspace: WORKSPACE.into(),
                operation: CommitOperation::WorkspaceMembershipCommand {
                    invocation: uuid::Uuid::new_v4(),
                    caller: cert_c,
                    command: WorkspaceMembershipCommand::Settings(
                        tilo_peer_state::workspace_membership::SettingsChange::Rename {
                            name: "C's workspace".into(),
                        },
                    ),
                },
            },
            &key_c,
            now,
        )
        .await
        .is_err(),
        "the removed device's writes are refused"
    );
}

/// An invitation is the issuer's own link: the person who made it lists it
/// while it is redeemable and cancels it from their own device, after which
/// the link admits nobody, and another person's device cancels nothing.
#[tokio::test]
async fn an_invitation_is_listed_and_cancelled_from_the_issuers_device() {
    use tilo_peer_state::join::invitations::CancelInvitationRequest;

    let directory = tempfile::tempdir().unwrap();
    let (_root_a, key_a, cert_a) = device(61, 62, "A's desktop");
    let (_root_b, key_b, cert_b) = device(63, 64, "B's desktop");
    let workspace = WorkspaceId::parse(WORKSPACE).unwrap();
    let now = SystemTime::now();

    // A's device made the workspace; B is a member of it on their own device.
    let mut governor =
        TestGovernor::empty_interactive(cert_a.clone(), key_a.clone(), WORKSPACE).unwrap();
    governor.pin_clock(now);
    let a = open_file(&directory.path().join("a.json")).unwrap();
    enroll(
        &a,
        &key_a,
        &cert_a,
        &mut governor,
        MemberRole::Moderator,
        [11; 32],
        now,
    );
    // A Moderator moderates; inviting is governance B is given as an
    // administrator of the workspace.
    governor
        .allow_governance(
            &hex::encode(cert_b.user_public_key),
            tilo_action_policy::roles::governance::INVITE,
        )
        .unwrap();
    let b = open_file(&directory.path().join("b.json")).unwrap();
    enroll(
        &b,
        &key_b,
        &cert_b,
        &mut governor,
        MemberRole::Moderator,
        [12; 32],
        now,
    );
    for (bytes, _) in governor.outgoing() {
        a.receive(
            workspace,
            TransportOrigin::RetainedRelayDelta(cert_a.device_public_key),
            postcard::from_bytes(bytes).unwrap(),
            now,
        )
        .unwrap();
    }

    // B invites someone from their own device.
    let issued = b
        .issue_invitation(
            workspace,
            &cert_b,
            &IssueInvitationRequest {
                request_id: [13; 32],
                role: MemberRole::Viewer,
                recipient_user_public_key: None,
                invitee_name: Some("Ada".into()),
                expires_at_ns: testing::ns(now)
                    .unwrap()
                    .checked_add(60_000_000_000)
                    .unwrap(),
                inviter_name: None,
            },
            &[hint(&cert_b, &key_b)],
            &key_b,
            now,
        )
        .await
        .unwrap();
    let pending = b.pending_invitations(workspace, now).unwrap();
    assert_eq!(pending.len(), 1, "B's one invitation is still open");
    assert_eq!(pending[0].invite_id, issued.invite_id);
    assert_eq!(pending[0].role, MemberRole::Viewer);
    assert_eq!(pending[0].invitee_name.as_deref(), Some("Ada"));

    // What B publishes reaches A; the invitation itself is B's own bookkeeping.
    for message in outgoing(&b, workspace, now) {
        a.receive(
            workspace,
            TransportOrigin::RelayDelta(cert_b.device_public_key),
            message,
            now,
        )
        .unwrap();
    }
    assert!(
        a.pending_invitations(workspace, now).unwrap().is_empty(),
        "the link is B's, listed and answered at B's device"
    );

    // Another person's device does not cancel B's link.
    let cancel = CancelInvitationRequest {
        request_id: [14; 32],
        invite_id: issued.invite_id,
    };
    assert!(
        a.cancel_invitation(workspace, &cancel, &key_a, now)
            .is_err(),
        "an invitation is cancelled by the person who issued it"
    );

    // B cancels it from their own device, and the link admits nobody.
    assert!(
        b.cancel_invitation(workspace, &cancel, &key_b, now)
            .unwrap(),
        "B's device cancels the link it made"
    );
    assert!(
        b.pending_invitations(workspace, now).unwrap().is_empty(),
        "the link is no longer redeemable where it was made"
    );
    assert!(
        !b.cancel_invitation(workspace, &cancel, &key_b, now)
            .unwrap(),
        "asking again changes nothing"
    );
    let (_root_c, _key_c, cert_c) = device(65, 66, "C's desktop");
    let joined = JoinAnswer(OwnerJoinWorkspaces::new(b.clone(), key_b.clone()));
    let refused = answer(
        &joined,
        cert_c.device_public_key,
        JoinTransportRequest::Challenge(JoinChallengeRequest {
            signed_invitation: issued.signed_invitation,
            device_cert: cert_c,
            request_id: [15; 32],
        }),
        now,
    );
    assert!(
        matches!(refused, JoinTransportResponse::Error { .. }),
        "the cancelled link admits nobody: {refused:?}"
    );
    for message in outgoing(&b, workspace, now) {
        a.receive(
            workspace,
            TransportOrigin::RelayDelta(cert_b.device_public_key),
            message,
            now,
        )
        .unwrap();
    }
}

/// A device's direct link admits into its owner's live replica of the
/// workspace, and the owner publishes what it takes in: the device that dialled
/// holds, after the link alone, what the other device holds, on disk too.
#[tokio::test(flavor = "multi_thread")]
async fn a_link_admits_into_the_owner_and_the_owner_publishes_it() {
    use crate::common::{addr, localhost_endpoint};
    use tilo_peer_state::direct::{DIRECT_ALPN, DirectSync};

    let directory = tempfile::tempdir().unwrap();
    let (_root_a, key_a, cert_a) = device(91, 92, "A's desktop");
    let (_root_b, key_b, cert_b) = device(73, 74, "B's desktop");
    let workspace = WorkspaceId::parse(WORKSPACE).unwrap();
    let mut governor =
        TestGovernor::empty_interactive(cert_a.clone(), key_a.clone(), WORKSPACE).unwrap();
    let now = SystemTime::now();
    governor.pin_clock(now);
    let a = open_file(&directory.path().join("a.json")).unwrap();
    enroll(
        &a,
        &key_a,
        &cert_a,
        &mut governor,
        MemberRole::Moderator,
        [1; 32],
        now,
    );
    let b = open_file(&directory.path().join("b.json")).unwrap();
    enroll(
        &b,
        &key_b,
        &cert_b,
        &mut governor,
        MemberRole::Moderator,
        [2; 32],
        now,
    );
    // A's device takes what its host has published.
    for (bytes, _) in governor.outgoing() {
        a.receive(
            workspace,
            TransportOrigin::RetainedRelayDelta(cert_a.device_public_key),
            postcard::from_bytes(bytes).unwrap(),
            now,
        )
        .unwrap();
    }
    // A's device saves a shortcut of its own, as one of the workspace's
    // settings: a workspace change its own owner publishes, which B's device
    // has no way to have before the link.
    let mut saved = testing::shortcut("link");
    tilo_peer_state::workspace::sign_saved_shortcut(WORKSPACE, &mut saved, &cert_a, &key_a)
        .unwrap();
    a.commit_workspace_membership_command(
        OwnerCommitRequest {
            workspace: WORKSPACE.into(),
            operation: CommitOperation::WorkspaceMembershipCommand {
                invocation: uuid::Uuid::new_v4(),
                caller: cert_a.clone(),
                command: WorkspaceMembershipCommand::Settings(SettingsChange::Shortcuts(
                    tilo_automation::ShortcutChange::Save(Box::new(saved.clone())),
                )),
            },
        },
        &key_a,
        now,
    )
    .await
    .unwrap();
    let held = a
        .with_current(workspace, |view| Ok(view.peer().workspace_heads()))
        .unwrap();
    assert_ne!(
        b.with_current(workspace, |view| Ok(view.peer().workspace_heads()))
            .unwrap(),
        held,
        "B's device has not taken the workspace's newest change yet"
    );

    // A's device serves the workspace on the direct link; B's device dials it
    // and admits every frame into its own owner.
    let a_link = DirectSync::over(a.replica(workspace).unwrap(), key_a.clone())
        .owning(a.clone(), workspace)
        .unwrap();
    let b_link = DirectSync::over(b.replica(workspace).unwrap(), key_b.clone())
        .owning(b.clone(), workspace)
        .unwrap();
    let a_endpoint = localhost_endpoint(&key_a).await;
    let _router = iroh::protocol::Router::builder(a_endpoint.clone())
        .accept(DIRECT_ALPN, a_link)
        .spawn();
    let b_endpoint = localhost_endpoint(&key_b).await;
    // B's device retires and republishes its own custody while the link admits
    // frames: neither takes the other's away.
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let retiring = std::thread::spawn({
        let b = b.clone();
        let stop = std::sync::Arc::clone(&stop);
        move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                b.retire_outgoing(workspace).unwrap();
                b.persist(workspace).unwrap();
            }
        }
    });
    let link = tokio::spawn({
        let a_addr = addr(&a_endpoint);
        let dialing = b_link.clone();
        async move { dialing.connect(&b_endpoint, a_addr).await }
    });

    // B's committed view converges on A's: what the link admitted is published.
    let mut changes = b_link.changes();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while b
            .with_current(workspace, |view| Ok(view.peer().workspace_heads()))
            .unwrap()
            != held
        {
            changes.changed().await.unwrap();
        }
    })
    .await
    .expect("B's owner publishes what the link admitted");
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    retiring.join().unwrap();

    // And its record carries it: a fresh owner over the same file holds the
    // workspace's state with no link at all.
    let reopened = open_file(&directory.path().join("b.json")).unwrap();
    assert_eq!(
        reopened
            .with_current(workspace, |view| Ok(view.peer().workspace_heads()))
            .unwrap(),
        held,
        "the link's frames are in B's record"
    );
    assert_eq!(
        reopened
            .with_current(workspace, |view| Ok(view
                .peer()
                .workspace()
                .workspace_shortcuts()))
            .unwrap(),
        vec![tilo_automation::SavedShortcut::encode(&saved)],
        "the shortcut A saved is one of the workspace's records on B's device"
    );
    link.abort();
}
