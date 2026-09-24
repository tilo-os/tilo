use ed25519_dalek::SigningKey;
use qos_peer_state::join::{
    JoinChallengeRequest, JoinCompleteRequest, JoinInspection, JoinTransportRequest,
    JoinTransportResponse, MemberRole, MembershipChallengeRequest, sign_join_proof,
};
use qos_session_peer::{
    SessionOwner,
    collaboration::{
        CancelInvitationRequest, CollaborationService, IssueInvitationRequest, RemoveMemberRequest,
        SetMemberRoleRequest,
    },
};
use tilo_relay::workspace_join::{
    WORKSPACE_JOIN_ALPN, WorkspaceJoinClient, WorkspaceJoinProtocol, WorkspaceJoinService,
};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

fn service() -> CollaborationService {
    CollaborationService::new(vec![])
}

fn install_test_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
}

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .try_into()
        .unwrap()
}

fn guest(now: u64) -> (SigningKey, DeviceCert) {
    let root = SigningKey::from_bytes(&[31; 32]);
    let device = SigningKey::from_bytes(&[32; 32]);
    let mut certificate = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Independent guest".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: now.saturating_sub(1),
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut certificate, &root).unwrap();
    (device, certificate)
}

fn issue_request() -> IssueInvitationRequest {
    let now = now_ns();
    IssueInvitationRequest {
        request_id: [7; 32],
        role: MemberRole::Commenter,
        recipient_user_public_key: Some([8; 32]),
        invitee_name: Some("Invited person".into()),
        expires_at_ns: now + 20_000_000_000,
    }
}

#[test]
fn proof_redemption_role_and_removal_are_durable_and_single_use() {
    install_test_tracing();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("owner");
    let mut owner = SessionOwner::initialize(&root).unwrap();
    let administrator = owner.peer().authority().device_public_key;
    let now = now_ns();
    let (guest_key, guest) = guest(now);
    let service = service();
    let issued = service
        .issue(
            &mut owner,
            administrator,
            IssueInvitationRequest {
                request_id: [41; 32],
                role: MemberRole::Editor,
                recipient_user_public_key: Some(guest.user_public_key),
                invitee_name: Some("Guest person".into()),
                expires_at_ns: now + 120_000_000_000,
            },
            now,
        )
        .unwrap();
    let challenge = service
        .challenge(
            &mut owner,
            guest.device_public_key,
            JoinChallengeRequest {
                signed_invitation: issued.signed_invitation.clone(),
                device_cert: guest.clone(),
                request_id: [42; 32],
            },
            now + 1,
        )
        .unwrap()
        .challenge;
    let proof = sign_join_proof(challenge, &guest_key).unwrap();
    let completion = service
        .complete(
            &mut owner,
            guest.device_public_key,
            JoinCompleteRequest {
                proof: proof.clone(),
            },
            now + 2,
        )
        .unwrap();
    assert_eq!(completion.bootstrap.device_cert, guest);
    assert_eq!(completion.bootstrap.role, MemberRole::Editor);
    assert!(
        completion
            .access
            .member_user_roots
            .contains(&guest.user_public_key)
    );

    let guest_root = SigningKey::from_bytes(&[31; 32]);
    let second_device_key = SigningKey::from_bytes(&[33; 32]);
    let mut second_device = DeviceCert {
        user_public_key: guest.user_public_key,
        device_public_key: second_device_key.verifying_key().to_bytes(),
        device_label: "Independent guest second device".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: now.saturating_sub(1),
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut second_device, &guest_root).unwrap();
    let conflicting_challenge = service
        .challenge(
            &mut owner,
            second_device.device_public_key,
            JoinChallengeRequest {
                signed_invitation: issued.signed_invitation.clone(),
                device_cert: second_device.clone(),
                request_id: [46; 32],
            },
            now + 3,
        )
        .unwrap()
        .challenge;
    let conflicting = service
        .complete(
            &mut owner,
            second_device.device_public_key,
            JoinCompleteRequest {
                proof: sign_join_proof(conflicting_challenge, &second_device_key).unwrap(),
            },
            now + 4,
        )
        .unwrap_err();
    assert!(
        conflicting.to_string().contains("already redeemed"),
        "unexpected conflicting redemption result: {conflicting}"
    );
    let denied_admin = service
        .issue(
            &mut owner,
            guest.device_public_key,
            IssueInvitationRequest {
                request_id: [47; 32],
                role: MemberRole::Viewer,
                recipient_user_public_key: None,
                invitee_name: None,
                expires_at_ns: now + 60_000_000_000,
            },
            now + 4,
        )
        .unwrap_err();
    assert!(
        denied_admin.to_string().contains("Owner authority"),
        "unexpected guest administration result: {denied_admin}"
    );

    drop(owner);
    let mut owner = SessionOwner::open(&root).unwrap();
    let retry = service
        .complete(
            &mut owner,
            guest.device_public_key,
            JoinCompleteRequest {
                proof: proof.clone(),
            },
            now + 5,
        )
        .unwrap();
    assert_eq!(retry.bootstrap, completion.bootstrap);
    let wrong_endpoint = service
        .complete(
            &mut owner,
            [99; 32],
            JoinCompleteRequest {
                proof: proof.clone(),
            },
            now + 5,
        )
        .unwrap_err();
    assert!(wrong_endpoint.to_string().contains("different proof"));

    service
        .set_member_role(
            &mut owner,
            administrator,
            SetMemberRoleRequest {
                request_id: [43; 32],
                user_public_key: guest.user_public_key,
                role: MemberRole::Viewer,
            },
            now + 6,
        )
        .unwrap();
    drop(owner);

    let mut owner = SessionOwner::open(&root).unwrap();
    let member = owner
        .peer()
        .governed_members(std::time::SystemTime::now())
        .unwrap()
        .into_iter()
        .find(|member| member.user_public_key == guest.user_public_key)
        .unwrap();
    assert_eq!(member.role, MemberRole::Viewer);
    assert!(!member.removed);

    service
        .remove_member(
            &mut owner,
            administrator,
            RemoveMemberRequest {
                request_id: [44; 32],
                user_public_key: guest.user_public_key,
            },
            now + 7,
        )
        .unwrap();
    drop(owner);

    let mut owner = SessionOwner::open(&root).unwrap();
    let room = owner.peer().room().to_owned();
    let workspace_id = owner.workspace_id();
    let denied = service
        .membership_challenge(
            &mut owner,
            guest.device_public_key,
            MembershipChallengeRequest {
                room,
                workspace_id,
                device_cert: guest.clone(),
                request_id: [45; 32],
            },
            now + 8,
        )
        .unwrap_err();
    assert!(
        denied.to_string().contains("removed"),
        "unexpected denial: {denied}"
    );
    let cached_after_removal = service
        .complete(
            &mut owner,
            guest.device_public_key,
            JoinCompleteRequest { proof },
            now + 9,
        )
        .unwrap_err();
    assert!(
        cached_after_removal.to_string().contains("removed")
            || cached_after_removal.to_string().contains("permission")
            || cached_after_removal
                .to_string()
                .contains("no room membership"),
        "cached admission bypassed removal: {cached_after_removal}"
    );
}

#[test]
fn cancellation_after_challenge_prevents_redemption() {
    install_test_tracing();
    let temp = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
    let administrator = owner.peer().authority().device_public_key;
    let now = now_ns();
    let (guest_key, guest) = guest(now);
    let service = service();
    let issued = service
        .issue(
            &mut owner,
            administrator,
            IssueInvitationRequest {
                request_id: [51; 32],
                role: MemberRole::Viewer,
                recipient_user_public_key: Some(guest.user_public_key),
                invitee_name: None,
                expires_at_ns: now + 120_000_000_000,
            },
            now,
        )
        .unwrap();
    let challenge = service
        .challenge(
            &mut owner,
            guest.device_public_key,
            JoinChallengeRequest {
                signed_invitation: issued.signed_invitation,
                device_cert: guest.clone(),
                request_id: [52; 32],
            },
            now + 1,
        )
        .unwrap()
        .challenge;
    service
        .cancel(
            &mut owner,
            administrator,
            CancelInvitationRequest {
                request_id: [53; 32],
                invite_id: issued.invite_id,
            },
        )
        .unwrap();
    let denied = service
        .complete(
            &mut owner,
            guest.device_public_key,
            JoinCompleteRequest {
                proof: sign_join_proof(challenge, &guest_key).unwrap(),
            },
            now + 2,
        )
        .unwrap_err();
    assert!(denied.to_string().contains("cancelled"));
    assert!(
        !owner
            .peer()
            .governed_access(std::time::SystemTime::now())
            .unwrap()
            .member_user_roots
            .contains(&guest.user_public_key)
    );
}

#[test]
fn issuance_and_cancellation_are_restart_stable_and_request_bound() {
    install_test_tracing();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("owner");
    let mut owner = SessionOwner::initialize(&root).unwrap();
    let administrator = owner.peer().authority().device_public_key;
    let host_service = owner.host_certificate().device_public_key;
    let service = service();
    let request = issue_request();
    let first_expiry = request.expires_at_ns;
    let now = now_ns();

    let first = service
        .issue(&mut owner, administrator, request.clone(), now)
        .unwrap();
    let host_service_admin = service
        .issue(
            &mut owner,
            host_service,
            IssueInvitationRequest {
                request_id: [11; 32],
                ..issue_request()
            },
            now + 1,
        )
        .unwrap_err();
    assert!(host_service_admin.to_string().contains("Owner authority"));
    let retry = service
        .issue(&mut owner, administrator, request.clone(), now + 1)
        .unwrap();
    assert_eq!(retry, first, "an exact retry must return the same bearer");
    assert_eq!(
        service
            .pending(&owner, administrator, now + 1)
            .unwrap()
            .len(),
        1
    );

    let mut changed = request.clone();
    changed.role = MemberRole::Editor;
    let error = service
        .issue(&mut owner, administrator, changed, now + 1)
        .unwrap_err();
    assert!(error.to_string().contains("request ID"));

    drop(owner);
    let mut reopened = SessionOwner::open(&root).unwrap();
    assert_eq!(
        service
            .issue(&mut reopened, administrator, request, now + 2)
            .unwrap(),
        first
    );

    service
        .cancel(
            &mut reopened,
            administrator,
            CancelInvitationRequest {
                request_id: [9; 32],
                invite_id: first.invite_id,
            },
        )
        .unwrap();
    drop(reopened);

    let mut reopened = SessionOwner::open(&root).unwrap();
    service
        .cancel(
            &mut reopened,
            administrator,
            CancelInvitationRequest {
                request_id: [9; 32],
                invite_id: first.invite_id,
            },
        )
        .unwrap();
    assert!(
        service
            .pending(&reopened, administrator, now + 3)
            .unwrap()
            .is_empty()
    );
    assert!(
        service
            .inspect(&reopened, &first.signed_invitation, now + 3)
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
    assert_eq!(
        service
            .issue(
                &mut reopened,
                administrator,
                IssueInvitationRequest {
                    request_id: [7; 32],
                    role: MemberRole::Commenter,
                    recipient_user_public_key: Some([8; 32]),
                    invitee_name: Some("Invited person".into()),
                    expires_at_ns: first_expiry,
                },
                first_expiry + 1,
            )
            .unwrap(),
        first
    );
    let second = service
        .issue(
            &mut reopened,
            administrator,
            IssueInvitationRequest {
                request_id: [10; 32],
                ..issue_request()
            },
            now + 4,
        )
        .unwrap();
    let reused_cancel = service
        .cancel(
            &mut reopened,
            administrator,
            CancelInvitationRequest {
                request_id: [9; 32],
                invite_id: second.invite_id,
            },
        )
        .unwrap_err();
    assert!(reused_cancel.to_string().contains("request ID"));
}

#[derive(Clone)]
struct SerializedTestHost {
    owner: std::sync::Arc<tokio::sync::Mutex<SessionOwner>>,
    service: CollaborationService,
}

impl std::fmt::Debug for SerializedTestHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SerializedTestHost")
    }
}

impl WorkspaceJoinService for SerializedTestHost {
    async fn handle(
        &self,
        remote_endpoint: [u8; 32],
        request: JoinTransportRequest,
    ) -> JoinTransportResponse {
        let mut owner = self.owner.lock().await;
        let now = now_ns();
        let result = match request {
            JoinTransportRequest::Inspect { signed_invitation } => self
                .service
                .inspect(&owner, &signed_invitation, now)
                .map(|verified| {
                    JoinTransportResponse::Inspection(JoinInspection {
                        invite_id: verified.invite_id(),
                        grant: verified.grant().clone(),
                    })
                }),
            JoinTransportRequest::Challenge(request) => self
                .service
                .challenge(&mut owner, remote_endpoint, request, now)
                .map(JoinTransportResponse::Challenge),
            JoinTransportRequest::Complete(request) => self
                .service
                .complete(&mut owner, remote_endpoint, request, now)
                .map(|completion| JoinTransportResponse::Complete(completion.bootstrap)),
            _ => {
                return JoinTransportResponse::Error {
                    code: 400,
                    message: "unexpected test request".into(),
                };
            }
        };
        result.unwrap_or_else(|error| JoinTransportResponse::Error {
            code: 403,
            message: error.to_string(),
        })
    }
}

#[tokio::test]
async fn real_iroh_socket_redeems_independent_root_and_survives_restart() {
    install_test_tracing();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("owner");
    let mut owner = SessionOwner::initialize(&root).unwrap();
    let now = now_ns();
    let (guest_key, guest) = guest(now);
    let service = service();
    let administrator = owner.peer().authority().device_public_key;
    let issued = service
        .issue(
            &mut owner,
            administrator,
            IssueInvitationRequest {
                request_id: [71; 32],
                role: MemberRole::Commenter,
                recipient_user_public_key: Some(guest.user_public_key),
                invitee_name: Some("Socket guest".into()),
                expires_at_ns: now + 120_000_000_000,
            },
            now,
        )
        .unwrap();
    let owner = std::sync::Arc::new(tokio::sync::Mutex::new(owner));
    let server = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap();
    let router = iroh::protocol::Router::builder(server.clone())
        .accept(
            WORKSPACE_JOIN_ALPN,
            WorkspaceJoinProtocol::new(SerializedTestHost {
                owner: owner.clone(),
                service,
            }),
        )
        .spawn();
    let guest_endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&guest_key.to_bytes()))
        .relay_mode(iroh::RelayMode::Disabled)
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap();
    let address = iroh::EndpointAddr::new(server.id()).with_ip_addr(std::net::SocketAddr::from((
        std::net::Ipv4Addr::LOCALHOST,
        server.bound_sockets().first().unwrap().port(),
    )));
    let bootstrap = WorkspaceJoinClient::new(guest_endpoint.clone())
        .redeem(
            address,
            issued.signed_invitation,
            guest.clone(),
            &guest_key,
            [72; 32],
        )
        .await
        .unwrap();
    assert_eq!(bootstrap.device_cert, guest);
    assert_eq!(bootstrap.role, MemberRole::Commenter);

    router.shutdown().await.unwrap();
    guest_endpoint.close().await;
    drop(owner);
    let reopened = SessionOwner::open(&root).unwrap();
    assert!(
        reopened
            .peer()
            .governed_access(std::time::SystemTime::now())
            .unwrap()
            .member_user_roots
            .contains(&bootstrap.device_cert.user_public_key)
    );
}
