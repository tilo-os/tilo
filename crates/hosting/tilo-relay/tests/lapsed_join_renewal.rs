//! A device lost its relay after it proved an invitation and before the
//! relay admitted it, and renewed its certificate since. Its proof lapses
//! with its challenge: the relay answers so, and the device asks for the
//! invitation again with the renewal and joins as the member it was
//! invited as.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the relay serves the whole test"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use ed25519_dalek::SigningKey;
use tilo_identity_host::custody::HostIdentity;
use tilo_peer_state::join::{
    self, JoinChallengeRequest, JoinCompleteRequest, JoinTransportRequest, JoinTransportResponse,
    MemberRole, TRANSPORT_IROH_WORKSPACE_HOST_ENDPOINT_ADDR_V1,
};
use tilo_relay::rooms::IssueInvitationRequest;
use tilo_relay::workspace_join::WorkspaceJoinClient;
use tilo_relay::{Relay, RelayConfig};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

// Integration tests re-exec this binary as the automerge worker.
tilo_automerge_isolate::test_worker!();

const SECOND: u64 = 1_000_000_000;

fn certificate(
    root: &SigningKey,
    key: &SigningKey,
    caps: DeviceCapabilities,
    issued_at_ns: u64,
    expires_at_ns: Option<u64>,
) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "device".into(),
        capabilities: caps,
        issued_at_ns,
        expires_at_ns,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lapsed_proof_made_before_a_renewal_is_asked_for_again_with_the_renewal() {
    let alice = SigningKey::from_bytes(&[1; 32]);
    let bob = SigningKey::from_bytes(&[2; 32]);
    let host_key = SigningKey::from_bytes(&[3; 32]);
    let laptop_key = SigningKey::from_bytes(&[4; 32]);
    let desk_key = SigningKey::from_bytes(&[5; 32]);
    let host = certificate(&alice, &host_key, DeviceCapabilities::host(), 1, None);
    let laptop = certificate(&alice, &laptop_key, DeviceCapabilities::primary(), 1, None);
    let now = tilo_clock::unix_now_ns();
    // Two thirds through its lifetime: still valid, and due for renewal.
    let due = certificate(
        &bob,
        &desk_key,
        DeviceCapabilities::primary(),
        now.saturating_sub(400 * SECOND),
        Some(now.saturating_add(200 * SECOND)),
    );
    let renewed = certificate(
        &bob,
        &desk_key,
        DeviceCapabilities::primary(),
        now,
        Some(now.saturating_add(600 * SECOND)),
    );
    // The relay's clock runs a challenge's lifetime and more behind until
    // it has challenged the device: the challenge it issues is past when
    // the device comes back, with no waiting.
    tilo_clock::freeze_wall(
        SystemTime::now()
            .checked_sub(Duration::from_secs(61))
            .unwrap(),
    );
    let state = tempfile::tempdir().unwrap();
    let relay = Relay::start(
        RelayConfig {
            state_root: state.path().to_path_buf(),
            ..RelayConfig::default()
        },
        tilo_relay::daemon::RelayIdentity {
            user_public_key: alice.verifying_key().to_bytes(),
            device_signing_key: host_key.clone(),
            device_cert: host.clone(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();
    let rooms = relay.rooms().unwrap();
    let room = rooms.create("Design review").await.unwrap();
    rooms.enter(&room.room, &laptop).await.unwrap();
    let issued = rooms
        .issue(
            &room.room,
            &laptop,
            IssueInvitationRequest {
                request_id: [9; 32],
                role: MemberRole::Editor,
                recipient_user_public_key: Some(bob.verifying_key().to_bytes()),
                invitee_name: None,
                expires_at_ns: now.saturating_add(600 * SECOND),
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let invitation = join::encode_invitation_text(&issued.signed_invitation);

    // Bob's device asks for a challenge with the certificate it holds and
    // signs its proof, kept to be sent; the relay never receives it.
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&desk_key.to_bytes()))
        .bind()
        .await
        .unwrap();
    let client = WorkspaceJoinClient::new(endpoint);
    let verified = join::decode_and_verify_invitation_text(
        &invitation,
        Some(bob.verifying_key().to_bytes()),
        &RevocationList::new(),
        now,
    )
    .unwrap();
    let hint = verified
        .grant()
        .transport
        .iter()
        .find(|hint| hint.kind == TRANSPORT_IROH_WORKSPACE_HOST_ENDPOINT_ADDR_V1)
        .cloned()
        .unwrap();
    let JoinTransportResponse::Challenge(challenge) = client
        .request(
            relay.node_addr(),
            JoinTransportRequest::Challenge(JoinChallengeRequest {
                signed_invitation: verified.signed_invitation().to_vec(),
                device_cert: due.clone(),
                request_id: [7; 32],
            }),
        )
        .await
        .unwrap()
    else {
        panic!("the relay challenges the device");
    };
    let directory = tempfile::tempdir().unwrap();
    let owner = tilo_joined_room::open_file(&directory.path().join("joined-rooms.json")).unwrap();
    tilo_joined_room::testing::retain_unsent_enrollment(
        &owner,
        verified,
        JoinCompleteRequest {
            proof: join::sign_join_proof(challenge.challenge, &desk_key).unwrap(),
        },
        due.clone(),
        hint,
    )
    .unwrap();

    // The device comes back with its certificate renewed, after the
    // challenge's lifetime on the relay's clock.
    tilo_clock::thaw_wall();
    let enrollment = tilo_joined_room::enroll::prepare_and_complete(
        client,
        HostIdentity {
            device_cert: renewed.clone(),
            device_signing_key: desk_key.clone(),
            workspace_device_certs: HashMap::default(),
            workspace_revocations: HashMap::default(),
            identity_paths: None,
        },
        room.workspace_id,
        None,
        invitation,
        owner.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        enrollment.bootstrap().device_cert,
        renewed,
        "admitted with the renewal"
    );
    assert!(
        enrollment.bootstrap().certified_roster.contains(&renewed)
            && !enrollment.bootstrap().certified_roster.contains(&due),
        "the relay holds the renewal, never the certificate it replaced"
    );
    owner.install(&enrollment, SystemTime::now()).unwrap();
    let joined = tilo_desktop_model::RoomId::parse(&room.room).unwrap();
    let role = owner
        .with_current(joined, |view| {
            Ok(view
                .peer()
                .permissions(desk_key.verifying_key().to_bytes(), SystemTime::now())
                .ok()
                .map(|member| member.role))
        })
        .unwrap();
    assert_eq!(role, Some(collab_types::Role::Editor), "the invited member");
}
