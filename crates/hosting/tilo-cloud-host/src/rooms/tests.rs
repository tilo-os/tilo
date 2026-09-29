//! A Cloud host keeps its person in the Tilo Community and opens their
//! direct room with the team, against a local relay hosting the community
//! the way `tilo-relay-admin community` sets one up.
use super::*;
use tilo_relay::{
    Relay, RelayClient, RelayCoeditSession, RelayConfig, TrustPolicy, WorkspaceBinding,
    public_room::{HostedStandingInvite, PublicRoomHosting},
};
use tilo_room::{NeighbourMsg, SeedState, StatePeer, TransportOrigin};
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

/// Bounds a wait that ends as soon as its condition holds.
const WITHIN: Duration = Duration::from_secs(60);

fn certified(
    root: &SigningKey,
    key: &SigningKey,
    caps: DeviceCapabilities,
    label: &str,
) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: caps,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

/// The team's public room, hosted on a relay its owner delegated
/// Commenter admission to, with one standing invitation.
struct Community {
    relay: Relay,
    room: RoomId,
    token: String,
    team_root: SigningKey,
    team_key: SigningKey,
    team_cert: DeviceCert,
    _state: tempfile::TempDir,
}

impl Community {
    fn team_person(&self) -> [u8; 32] {
        self.team_root.verifying_key().to_bytes()
    }

    /// Point a release's community configuration at this community.
    fn configure(&self, dir: &Path) -> PathBuf {
        let path = dir.join("community.toml");
        std::fs::write(
            &path,
            format!(
                "[community]\ninvite = \"{}\"\n\n[team]\nuser_public_key = \"{}\"\n",
                self.token,
                hex::encode(self.team_person())
            ),
        )
        .unwrap();
        path
    }

    /// The team's device, subscribed to `workspace` on the community host.
    async fn team_session(&self, workspace: u64) -> RelayCoeditSession {
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .bind()
            .await
            .unwrap();
        let client = RelayClient::connect_to_pinned_host(
            endpoint,
            self.relay.node_addr(),
            self.team_cert.clone(),
            self.team_key.clone(),
            RevocationList::new(),
        )
        .await
        .unwrap();
        RelayCoeditSession::join(client, workspace, self.team_cert.device_public_key).unwrap()
    }
}

fn relay_config(state: &Path, listen: std::net::SocketAddr, workspace: u64) -> RelayConfig {
    RelayConfig {
        state_root: state.to_path_buf(),
        iroh_listen: listen,
        trust_policy: TrustPolicy::AllowAny,
        workspaces: vec![WorkspaceBinding {
            id: workspace,
            pin: false,
            replication: tilo_relay::ReplicationIntent::FullWorkspace,
            admitted_users: Vec::new(),
            shell_authority: None,
            checkpoint_prefix_recovery: tilo_relay::CheckpointPrefixRecovery::default(),
        }],
        ..RelayConfig::default()
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one host, one owner and one standing invitation, set up in order"
)]
async fn hosted_community() -> Community {
    let operator = SigningKey::from_bytes(&[0xC1; 32]);
    let host_key = SigningKey::from_bytes(&[0xC3; 32]);
    let host_cert = certified(
        &operator,
        &host_key,
        DeviceCapabilities::host(),
        "community host",
    );
    let team_root = SigningKey::from_bytes(&[0xC2; 32]);
    let team_key = SigningKey::from_bytes(&[0xC4; 32]);
    let team_cert = certified(
        &team_root,
        &team_key,
        DeviceCapabilities::primary(),
        "team laptop",
    );
    let identity = || tilo_relay::daemon::RelayIdentity {
        user_public_key: operator.verifying_key().to_bytes(),
        device_signing_key: host_key.clone(),
        device_cert: host_cert.clone(),
        revocations: RevocationList::new(),
    };

    // The team's room: public, owned and governed by the team's device.
    let room = RoomId::parse(&uuid::Uuid::new_v4().to_string()).unwrap();
    let room_id = room.to_string();
    let workspace = tilo_room::room_workspace_id(&room_id).unwrap();
    let now = SystemTime::now();
    let mut changes =
        tilo_room::seed_room(&room_id, &team_cert, std::slice::from_ref(&team_cert)).unwrap();
    let mut doc = automerge::AutoCommit::new().with_actor(automerge::ActorId::from(
        team_cert.device_public_key.to_vec(),
    ));
    for change in &changes {
        tilo_automerge::ApplyIncoming::load_incoming(&mut doc, change).unwrap();
    }
    let before = doc.get_heads();
    tilo_desktop_model::projection::project_channel_class(
        &mut doc,
        collab_types::ChannelClass::Public,
    );
    doc.commit();
    changes.push(doc.save_after(&before));
    let mut team = StatePeer::new(
        &room_id,
        team_cert.clone(),
        vec![team_cert.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    team.install_seed(
        SeedState {
            room_changes: changes,
            documents: Vec::new(),
        },
        &team_key,
        now,
    )
    .unwrap();

    // The host, on a fixed port so the invitation's address survives the
    // restart that picks up the hosting record.
    let state = tempfile::tempdir().unwrap();
    let listen = std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let relay = Relay::start(relay_config(state.path(), listen, workspace), identity())
        .await
        .unwrap();
    let address = relay.node_addr();
    relay.shutdown().await;
    let team_iroh = iroh::SecretKey::from_bytes(&team_key.to_bytes());
    let mut draft = neighbour_wire::invite::InviteDraft::new(
        workspace,
        room_id.clone(),
        iroh::EndpointAddr::new(team_iroh.public()),
        neighbour_wire::invite::InviteRole::Commenter,
        u64::MAX,
    );
    draft.owner_device_cert = Some(team_cert.clone());
    draft.host_addrs = vec![address];
    draft.standing = true;
    let token = neighbour_wire::invite::mint_invite(&team_iroh, draft).unwrap();
    let delegation = collab_types::mint_root(
        collab_types::InviteSpec {
            subject: collab_types::Ed25519PublicKey::from_bytes(host_cert.device_public_key),
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
        &team_root,
    )
    .unwrap();
    let store = tilo_relay::store::WorkspaceStore::open(state.path(), workspace, false).unwrap();
    PublicRoomHosting {
        room: room_id.clone(),
        owner: team_root.verifying_key().to_bytes(),
        delegation: vec![delegation],
        invites: vec![HostedStandingInvite {
            id: hex::encode(neighbour_wire::invite::invite_id(&token)),
            token: token.clone(),
            revoked: false,
        }],
    }
    .save(store.root())
    .unwrap();
    let relay = Relay::start(relay_config(state.path(), listen, workspace), identity())
        .await
        .unwrap();

    let community = Community {
        relay,
        room,
        token,
        team_root,
        team_key,
        team_cert,
        _state: state,
    };
    // The team connects once, which leaves its identity and governance
    // retained at the host, then goes away.
    let session = community.team_session(workspace).await;
    for message in [
        NeighbourMsg::IdentityHello {
            cert: Box::new(community.team_cert.clone()),
            revocations: Vec::new(),
        },
        team.room_message(),
    ] {
        let (channel, _) = tilo_room::transport::retention(&message, true).unwrap();
        let body = postcard::to_stdvec(&message).unwrap();
        session
            .publish_covering_checkpoint(
                channel,
                *blake3::hash(&body).as_bytes(),
                tilo_relay::coedit::Coverage::Supersedes,
                body,
            )
            .unwrap();
    }
    let retained = tilo_test_env::within("the team's state retained at the host", async {
        while store.load_retained().map_or(0, |retained| retained.len()) < 2 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    retained.await;
    session.close().await;
    community
}

/// A Cloud host's rooms, over the enrolled Store in `dir`, with the release's
/// community configured at `config`.
async fn cloud_rooms(
    dir: &Path,
    identity: &Identity,
    config: PathBuf,
) -> (
    RoomsHandle,
    tokio::sync::watch::Sender<bool>,
    tokio::task::JoinHandle<()>,
) {
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .bind()
        .await
        .unwrap();
    let rooms = Rooms::open(dir, identity.clone(), config, endpoint).unwrap();
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let (handle, task) = rooms.start(stopped);
    (handle, stop, task)
}

/// Wait until `done` holds for the rooms' status.
async fn until(
    handle: &RoomsHandle,
    what: &str,
    done: impl Fn(&RoomsStatus) -> bool,
) -> RoomsStatus {
    let started = Instant::now();
    loop {
        let status = handle.status().await.unwrap();
        if done(&status) {
            return status;
        }
        assert!(started.elapsed() < WITHIN, "{what}: {status:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn community_holding(status: &RoomsStatus) -> &'static str {
    status
        .community
        .as_ref()
        .map_or("unconfigured", |community| community.holding)
}

/// Everyone is in the Tilo Community by default: the Cloud host joins it on
/// its own and opens the person's direct room with the team, where the team
/// is an Editor. The person leaves with one action, and stays out across a
/// restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_cloud_host_joins_the_community_opens_the_team_room_and_a_person_who_left_stays_out() {
    let community = Box::pin(hosted_community()).await;
    let (dir, store, _) = crate::agent_owner_tests::enrolled();
    let identity = store.rooms_identity().unwrap();
    drop(store);
    let config = community.configure(dir.path());
    let (handle, stop, task) = cloud_rooms(dir.path(), &identity, config.clone()).await;

    let joined = until(
        &handle,
        "the community joined and the team room opened",
        |status| {
            community_holding(status) == "admitted"
                && status.rooms.iter().any(|room| room.kind == "direct")
        },
    )
    .await;
    let community_room = joined
        .rooms
        .iter()
        .find(|room| room.kind == "community")
        .expect("the community room is held");
    assert_eq!(community_room.room, community.room.to_string());
    assert_eq!(
        community_room.governor, community.team_cert,
        "pinned to the team's device"
    );
    assert_eq!(
        joined.community.as_ref().unwrap().membership,
        "auto",
        "joining was automatic"
    );
    let direct = joined
        .rooms
        .iter()
        .find(|room| room.kind == "direct")
        .unwrap()
        .clone();
    assert_eq!(
        direct.governor, identity.cert,
        "the Cloud host governs the team room"
    );
    assert_eq!(
        direct.peer_person,
        Some(hex::encode(community.team_person()))
    );
    assert_eq!(
        direct.room,
        tilo_reference::DirectRoom::between(
            &hex::encode(identity.cert.user_public_key),
            &hex::encode(community.team_person())
        )
        .unwrap()
        .room_id(),
        "the room both ends derive"
    );

    // The team's end of the direct room: it receives the Cloud host's
    // governance, holds Editor there, and its hello reaches the Cloud host.
    let direct_room = RoomId::parse(&direct.room).unwrap();
    let workspace: u64 = direct.workspace_id.parse().unwrap();
    let team = community.team_session(workspace).await;
    let mut team_peer = StatePeer::new(
        &direct.room,
        identity.cert.clone(),
        vec![identity.cert.clone(), community.team_cert.clone()],
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    tilo_test_env::within("the direct room's governance at the team", async {
        while team_peer
            .permissions(community.team_cert.device_public_key, SystemTime::now())
            .is_err()
        {
            let Some(Inbound::Delta { author, data }) = team.recv().await else {
                continue;
            };
            if let Ok(message) = postcard::from_bytes::<NeighbourMsg>(&data) {
                let _ = team_peer.receive_from(
                    TransportOrigin::RetainedRelayDelta(author),
                    message,
                    SystemTime::now(),
                );
            }
        }
    })
    .await;
    assert_eq!(
        team_peer
            .permissions(community.team_cert.device_public_key, SystemTime::now())
            .unwrap()
            .0,
        collab_types::Role::Editor
    );
    let hello = NeighbourMsg::IdentityHello {
        cert: Box::new(community.team_cert.clone()),
        revocations: Vec::new(),
    };
    let (channel, _) = tilo_room::transport::retention(&hello, true).unwrap();
    let body = postcard::to_stdvec(&hello).unwrap();
    team.publish_covering_checkpoint(
        channel,
        *blake3::hash(&body).as_bytes(),
        tilo_relay::coedit::Coverage::Supersedes,
        body,
    )
    .unwrap();
    let rooms_store = FileRooms::new(dir.path());
    tilo_test_env::within(
        "the team's device admitted in the Cloud host's room",
        async {
            loop {
                let admitted = rooms_store
                    .held()
                    .unwrap()
                    .into_iter()
                    .find(|record| record.room == direct_room)
                    .and_then(|record| {
                        HeldRoom::restore(record, &identity.cert, SystemTime::now()).ok()
                    })
                    .is_some_and(|room| {
                        room.admits(community.team_cert.device_public_key, SystemTime::now())
                    });
                if admitted {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        },
    )
    .await;
    team.close().await;

    // Leaving is one action, and the host follows it.
    let left = handle
        .set_membership(settings_store::community::Membership::Left)
        .await
        .unwrap();
    assert_eq!(left.community.as_ref().unwrap().membership, "left");
    until(&handle, "the community left", |status| {
        community_holding(status) == "no"
            && !status.rooms.iter().any(|room| room.kind == "community")
    })
    .await;
    assert!(
        !rooms_store
            .held()
            .unwrap()
            .iter()
            .any(|record| record.room == community.room),
        "the room's store root is gone"
    );
    stop.send(true).unwrap();
    task.await.unwrap();

    // After a restart the person is still out, and still has the team room.
    let (handle, stop, task) = cloud_rooms(dir.path(), &identity, config).await;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(4) {
        let status = handle.status().await.unwrap();
        assert_eq!(
            community_holding(&status),
            "no",
            "never joined again: {status:?}"
        );
        assert_eq!(status.community.as_ref().unwrap().membership, "left");
        assert!(status.rooms.iter().any(|room| room.room == direct.room));
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Joining again is the person's choice.
    handle
        .set_membership(settings_store::community::Membership::Auto)
        .await
        .unwrap();
    until(&handle, "the community joined again", |status| {
        community_holding(status) == "admitted"
    })
    .await;
    stop.send(true).unwrap();
    task.await.unwrap();
}
