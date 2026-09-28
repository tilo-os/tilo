//! A relay hosting a public room its owner delegated admission to: any
//! certified stranger redeems the room's standing invitation and becomes a
//! member, the signed admission reaches every member through the room's
//! retained state, and no member can speak as another endpoint.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]

use std::time::Duration;

use tilo_relay::public_room::{HostedStandingInvite, PublicRoomHosting};
use tilo_relay::{
    AttributedPresence, CoeditPayload, Envelope, Relay, RelayClient, RelayConfig, TrustPolicy,
    WorkspaceBinding,
};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestDevice, TestUser, client_endpoint};

struct Community {
    relay: Relay,
    room: String,
    workspace: u64,
    invite_id: [u8; 32],
    owner: [u8; 32],
    state: tempfile::TempDir,
}

/// The team (the room's owner) mints the standing invitation from its own
/// device and delegates Commenter admission to the relay's device; the
/// operator registers both, exactly as `tilo-relay-admin community …` does.
async fn community() -> Community {
    let operator = TestUser::fresh("Tilo hosting");
    let host = operator.enrol("community-host", DeviceCapabilities::host());
    let team = TestUser::fresh("Tilo team");
    let team_device = team.enrol("team-laptop", DeviceCapabilities::primary());

    let room = uuid::Uuid::new_v4().to_string();
    let workspace = tilo_relay::room_workspace_id(&room).unwrap();
    let team_key = iroh_base::SecretKey::from_bytes(&team_device.sk.to_bytes());
    let mut draft = neighbour_wire::invite::InviteDraft::new(
        workspace,
        room.clone(),
        iroh_base::EndpointAddr::new(team_key.public()),
        neighbour_wire::invite::InviteRole::Commenter,
        u64::MAX,
    );
    draft.owner_device_cert = Some(team_device.cert.clone());
    draft.standing = true;
    let token = neighbour_wire::invite::mint_invite(&team_key, draft).unwrap();
    let invite_id = neighbour_wire::invite::invite_id(&token);
    let delegation = collab_types::mint_root(
        collab_types::InviteSpec {
            subject: collab_types::Ed25519PublicKey::from_bytes(host.device_pubkey()),
            workspace,
            role: collab_types::Role::Commenter,
            caps: [
                collab_types::caps::Verb::Read,
                collab_types::caps::Verb::Comment,
            ]
            .into_iter()
            .collect(),
            sub_resource: None,
            not_after: None,
        },
        &team.user_sk,
    )
    .unwrap();

    let state = tempfile::tempdir().unwrap();
    let store = tilo_relay::store::WorkspaceStore::open(state.path(), workspace, false).unwrap();
    PublicRoomHosting {
        room: room.clone(),
        owner: team.user_pk,
        delegation: vec![delegation],
        invites: vec![HostedStandingInvite {
            id: hex::encode(invite_id),
            token,
            revoked: false,
        }],
    }
    .save(store.root())
    .unwrap();
    let cfg = RelayConfig {
        state_root: state.path().to_path_buf(),
        trust_policy: TrustPolicy::AllowAny,
        workspaces: vec![WorkspaceBinding {
            id: workspace,
            pin: false,
            replication: tilo_relay::ReplicationIntent::FullWorkspace,
            admitted_users: Vec::new(),
            shell_authority: None,
            checkpoint_prefix_recovery: tilo_relay::CheckpointPrefixRecovery::Disabled,
        }],
        ..RelayConfig::default()
    };
    let relay = Relay::start(
        cfg,
        tilo_relay::daemon::RelayIdentity {
            user_public_key: operator.user_pk,
            device_signing_key: host.sk,
            device_cert: host.cert,
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();
    Community {
        relay,
        room,
        workspace,
        invite_id,
        owner: team.user_pk,
        state,
    }
}

async fn stranger(community: &Community, label: &str) -> (TestDevice, RelayClient, [u8; 32]) {
    let person = TestUser::fresh(label);
    let device = person.enrol(label, DeviceCapabilities::primary());
    let client = RelayClient::connect_to_pinned_host(
        client_endpoint().await,
        community.relay.node_addr(),
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("a certified stranger may connect to a public host");
    (device, client, person.user_pk)
}

async fn next(client: &RelayClient) -> Envelope {
    tilo_test_env::within("the relay's answer", client.recv())
        .await
        .expect("the connection stays open")
}

/// Drain until `Ready`, returning every room admission replayed before it.
async fn admissions_until_ready(client: &RelayClient) -> Vec<collab_types::HostAdmissionV1> {
    let mut admissions = Vec::new();
    loop {
        match next(client).await {
            Envelope::Ready { .. } => return admissions,
            Envelope::Delta { delta, .. } => {
                let Some(CoeditPayload::Checkpoint { body, .. }) = CoeditPayload::decode(&delta)
                else {
                    continue;
                };
                if let Ok(neighbour_wire::NeighbourMsg::RoomAdmission { admission, .. }) =
                    postcard::from_bytes(&body)
                {
                    admissions.push(*admission);
                }
            }
            _ => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stranger_redeems_the_standing_invite_and_every_member_can_verify_it() {
    let community = community().await;
    let (_ada, ada_client, ada_user) = stranger(&community, "Ada").await;

    ada_client
        .send(Envelope::Subscribe {
            workspace_id: community.workspace,
        })
        .unwrap();
    assert!(
        matches!(
            next(&ada_client).await,
            Envelope::NotAuthorised {
                refusal: tilo_relay::transport::Refusal::NotMember,
                ..
            }
        ),
        "without redeeming, a stranger is not a member, and asking again unchanged cannot help"
    );

    ada_client
        .send(Envelope::Redeem {
            workspace_id: community.workspace,
            invite_id: [0x99; 32],
        })
        .unwrap();
    assert!(
        matches!(
            next(&ada_client).await,
            Envelope::NotAuthorised { refusal: tilo_relay::transport::Refusal::NotMember, reason, .. }
                if reason.contains("not registered")
        ),
        "an unregistered invitation admits nobody"
    );

    ada_client
        .send(Envelope::Redeem {
            workspace_id: community.workspace,
            invite_id: community.invite_id,
        })
        .unwrap();
    assert!(matches!(
        next(&ada_client).await,
        Envelope::Admitted { workspace_id } if workspace_id == community.workspace
    ));
    ada_client
        .send(Envelope::Subscribe {
            workspace_id: community.workspace,
        })
        .unwrap();
    let replayed = admissions_until_ready(&ada_client).await;
    assert_eq!(replayed.len(), 1, "the redeemer receives its own admission");

    // A second stranger joins later and receives the first one's admission
    // from the room's retained state, verifiable against the room's owner
    // with nothing but the record itself.
    let (_bea, bea_client, _) = stranger(&community, "Bea").await;
    bea_client
        .send(Envelope::Redeem {
            workspace_id: community.workspace,
            invite_id: community.invite_id,
        })
        .unwrap();
    assert!(matches!(next(&bea_client).await, Envelope::Admitted { .. }));
    bea_client
        .send(Envelope::Subscribe {
            workspace_id: community.workspace,
        })
        .unwrap();
    let replayed = admissions_until_ready(&bea_client).await;
    let ada_admission = replayed
        .iter()
        .find(|admission| admission.member_user.as_bytes() == &ada_user)
        .expect("a late joiner learns earlier members from retained state");
    assert_eq!(ada_admission.room, community.room);
    assert_eq!(
        ada_admission.verify(
            collab_types::Ed25519PublicKey::from_bytes(community.owner),
            std::time::SystemTime::now()
        ),
        Ok(collab_types::Role::Commenter)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revoked_standing_invite_admits_nobody_new() {
    let community = community().await;
    let store =
        tilo_relay::store::WorkspaceStore::open(community.state.path(), community.workspace, false)
            .unwrap();
    let mut hosting = PublicRoomHosting::load(store.root()).unwrap().unwrap();
    hosting.invites[0].revoked = true;
    hosting.save(store.root()).unwrap();

    let (_cyd, cyd_client, _) = stranger(&community, "Cyd").await;
    cyd_client
        .send(Envelope::Redeem {
            workspace_id: community.workspace,
            invite_id: community.invite_id,
        })
        .unwrap();
    assert!(matches!(
        next(&cyd_client).await,
        Envelope::NotAuthorised { reason, .. } if reason.contains("revoked")
    ));
    cyd_client
        .send(Envelope::Subscribe {
            workspace_id: community.workspace,
        })
        .unwrap();
    assert!(matches!(
        next(&cyd_client).await,
        Envelope::NotAuthorised { .. }
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_cannot_publish_presence_as_another_endpoint() {
    let community = community().await;
    let (ada, ada_client, _) = stranger(&community, "Ada").await;
    let (_bea, bea_client, _) = stranger(&community, "Bea").await;
    for client in [&ada_client, &bea_client] {
        client
            .send(Envelope::Redeem {
                workspace_id: community.workspace,
                invite_id: community.invite_id,
            })
            .unwrap();
        assert!(matches!(next(client).await, Envelope::Admitted { .. }));
        client
            .send(Envelope::Subscribe {
                workspace_id: community.workspace,
            })
            .unwrap();
        admissions_until_ready(client).await;
    }

    let honest = AttributedPresence {
        author: ada.device_pubkey(),
        body: b"here".to_vec(),
    };
    ada_client
        .send(Envelope::Presence {
            workspace_id: community.workspace,
            payload: honest.encode().unwrap(),
        })
        .unwrap();
    let delivered = loop {
        if let Envelope::Presence { payload, .. } = next(&bea_client).await {
            break AttributedPresence::decode(&payload).unwrap();
        }
    };
    assert_eq!(
        delivered, honest,
        "presence attributed to its sender is delivered"
    );

    let forged = AttributedPresence {
        author: [0x42; 32],
        body: b"it was me".to_vec(),
    };
    ada_client
        .send(Envelope::Presence {
            workspace_id: community.workspace,
            payload: forged.encode().unwrap(),
        })
        .unwrap();
    // Anything already queued for Ada (Bea's admission, fanned out live) may
    // still arrive; the session must then end rather than carry on.
    tilo_test_env::within(
        "a forged presence author ending the forger's session",
        async {
            while let Some(envelope) = ada_client.recv().await {
                assert!(
                    !matches!(envelope, Envelope::Presence { .. }),
                    "the forger receives nothing further from the room"
                );
            }
        },
    )
    .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(500), bea_client.recv())
            .await
            .is_err(),
        "the forged presence never reaches another member"
    );
}
