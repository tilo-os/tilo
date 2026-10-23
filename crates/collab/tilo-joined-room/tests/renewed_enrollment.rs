//! An enrollment whose proof carries a certificate that is due for renewal
//! presents the renewal: the host ends up holding the certificate the
//! device presents from now on, not the one it is about to replace.

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
use std::time::SystemTime;

use ed25519_dalek::SigningKey;
use tilo_identity_host::custody::HostIdentity;
use tilo_peer_state::join::{self, MemberRole};
use tilo_relay::rooms::IssueInvitationRequest;
use tilo_relay::workspace_join::WorkspaceJoinClient;
use tilo_relay::{Relay, RelayConfig};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

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

fn custody(cert: &DeviceCert, key: &SigningKey) -> HostIdentity {
    HostIdentity {
        device_cert: cert.clone(),
        device_signing_key: key.clone(),
        workspace_device_certs: HashMap::default(),
        workspace_revocations: HashMap::default(),
        identity_paths: None,
    }
}

async fn endpoint(key: &SigningKey) -> iroh::Endpoint {
    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
        .bind()
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retained_proof_with_a_certificate_due_for_renewal_presents_the_renewal() {
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
    let invitation = rooms
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
    let invitation = join::encode_invitation_text(&invitation.signed_invitation);

    // Bob's device proves the invitation with the certificate it holds and
    // the host admits it, but the answer is never taken in: the proof stays
    // retained, to be sent again.
    let directory = tempfile::tempdir().unwrap();
    let owner = tilo_joined_room::open_file(&directory.path().join("joined-rooms.json")).unwrap();
    let client = WorkspaceJoinClient::new(endpoint(&desk_key).await);
    let unanswered = tilo_joined_room::enroll::prepare_and_complete(
        client.clone(),
        custody(&due, &desk_key),
        room.workspace_id,
        None,
        invitation.clone(),
        owner.clone(),
    )
    .await
    .unwrap();
    assert_eq!(unanswered.bootstrap().device_cert, due);

    // Its custody renews the certificate before it tries again.
    let enrollment = tilo_joined_room::enroll::prepare_and_complete(
        client,
        custody(&renewed, &desk_key),
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
        "the host answers for the renewal"
    );
    assert!(
        enrollment.bootstrap().certified_roster.contains(&renewed)
            && !enrollment.bootstrap().certified_roster.contains(&due),
        "the host holds the renewal in place of the certificate it replaced"
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
    assert_eq!(role, Some(collab_types::Role::Editor), "the same member");
}
