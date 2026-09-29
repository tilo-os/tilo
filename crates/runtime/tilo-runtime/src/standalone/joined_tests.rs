//! Joining a room a real durable host governs, as a foreign device.
#![allow(
    clippy::significant_drop_tightening,
    clippy::too_many_lines,
    reason = "one linear join scenario over a live host; its fixtures hold their locks and \
              endpoints for the whole test"
)]
use super::*;
use automerge::{AutoCommit, ROOT, transaction::Transactable};
use ed25519_dalek::SigningKey;
use std::sync::Arc;
use tilo_delivery_protocol::workspace_join::{
    IrohWorkspaceHostHintV1, WORKSPACE_JOIN_ALPN, WorkspaceHostMode, WorkspaceJoinService,
};
use tilo_relay::workspace_join::WorkspaceJoinProtocol;
use tilo_room::{
    join::{self, JoinTransportRequest, JoinTransportResponse, MemberRole},
    owner_commit::CommitOperation,
};
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, RevocationList, persist::IdentityPaths,
    persist::read_device_key, sign_device_cert,
};
use tilo_workspace_host::{
    SessionOwner,
    collaboration::{CollaborationService, IssueInvitationRequest},
};

type Authority = Arc<std::sync::Mutex<(SessionOwner, CollaborationService)>>;

/// What a sealed write says, which no file outside its key holders may hold.
const SECRET: &str = "only the room's key holders read this";

#[derive(Clone)]
struct JoinService(Authority);
impl std::fmt::Debug for JoinService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JoinService")
    }
}
impl WorkspaceJoinService for JoinService {
    async fn handle(
        &self,
        remote: [u8; 32],
        request: JoinTransportRequest,
    ) -> JoinTransportResponse {
        let now = ns(SystemTime::now());
        let mut authority = self.0.lock().unwrap();
        let (owner, service) = &mut *authority;
        let result = match request {
            JoinTransportRequest::Inspect { signed_invitation } => {
                service.inspect(owner, &signed_invitation, now).map(|join| {
                    JoinTransportResponse::Inspection(join::JoinInspection {
                        invite_id: join.invite_id(),
                        grant: join.grant().clone(),
                    })
                })
            }
            JoinTransportRequest::Challenge(request) => service
                .challenge(owner, remote, request, now)
                .map(JoinTransportResponse::Challenge),
            JoinTransportRequest::Complete(request) => service
                .complete(owner, remote, request, now)
                .map(|completion| JoinTransportResponse::Complete(completion.bootstrap)),
            _ => {
                return JoinTransportResponse::Error {
                    code: 400,
                    message: "unexpected membership operation".into(),
                };
            }
        };
        result.unwrap_or_else(|error| JoinTransportResponse::Error {
            code: 403,
            message: error.to_string(),
        })
    }
}

fn ns(at: SystemTime) -> u64 {
    u64::try_from(
        at.duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    )
    .unwrap()
}

async fn local_endpoint(key: &SigningKey) -> iroh::Endpoint {
    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
        .relay_mode(iroh::RelayMode::Disabled)
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_foreign_runtime_joins_a_hosted_room_as_an_editor_and_syncs_both_ways() {
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let mut governing = SessionOwner::initialize(&directory.path().join("authority")).unwrap();
    let governor = governing.peer().authority().clone();
    let governor_key =
        read_device_key(&IdentityPaths::under(governing.root().join("identity")).device_privkey)
            .unwrap();
    let host = governing.host_certificate().clone();
    let host_key = read_device_key(
        &IdentityPaths::under(governing.root().join("host-identity")).device_privkey,
    )
    .unwrap();
    let server = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap();
    let room = RoomId::parse(governing.peer().room()).unwrap();
    let topic = room_topic(room).unwrap();
    let hint = IrohWorkspaceHostHintV1 {
        version: 1,
        mode: WorkspaceHostMode::CertifiedHost,
        room: room.to_string(),
        workspace_id: topic,
        governing_authority: governor.clone(),
        endpoint_addr: server.addr(),
        host_user_public_key: host.user_public_key,
        host_device_certificate: host.clone(),
        packet_relay_url: "https://relay.invalid".into(),
    }
    .transport_hint(&host_key)
    .unwrap();

    // The runtime's device: certified by its person as a foreign device.
    let device_key = SigningKey::from_bytes(&[91; 32]);
    let root_key = SigningKey::from_bytes(&[92; 32]);
    let mut capabilities = DeviceCapabilities::member();
    capabilities.foreign = true;
    let mut device = DeviceCert {
        user_public_key: root_key.verifying_key().to_bytes(),
        device_public_key: device_key.verifying_key().to_bytes(),
        device_label: "Runtime on a laptop".into(),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut device, &root_key).unwrap();

    let service = CollaborationService::new(vec![hint.clone()]);
    let now = ns(SystemTime::now());
    let issued = service
        .issue(
            &mut governing,
            governor.device_public_key,
            IssueInvitationRequest {
                request_id: [93; 32],
                role: MemberRole::Editor,
                recipient_user_public_key: Some(device.user_public_key),
                invitee_name: Some("Runtime".into()),
                expires_at_ns: now + 60_000_000_000,
            },
            now,
        )
        .unwrap();
    let relay_root = directory.path().join("relay");
    let baseline = governing.peer().admitted_room().unwrap().clone().save();
    tilo_relay::store::WorkspaceStore::open(&relay_root, topic, true)
        .unwrap()
        .save_snapshot(&baseline)
        .unwrap();
    let authority: Authority = Arc::new(std::sync::Mutex::new((governing, service)));
    let relay = tilo_relay::Relay::start_on_endpoint_with_protocols(
        tilo_relay::RelayConfig {
            state_root: relay_root.clone(),
            identity_root: directory.path().join("relay-identity"),
            full_replica_status: directory.path().join("replica/status.json"),
            home_peer_bundle: directory.path().join("home-peer.toml"),
            trust_policy: tilo_relay::TrustPolicy::AllowList {
                users: vec![hex::encode(governor.user_public_key)],
            },
            workspaces: vec![tilo_relay::WorkspaceBinding {
                id: topic,
                pin: true,
                replication: tilo_relay::ReplicationIntent::FullWorkspace,
                admitted_users: vec![],
                shell_authority: Some(
                    tilo_room::retained_proof::RetentionAuthority::new(
                        &room.to_string(),
                        governor.clone(),
                    )
                    .unwrap(),
                ),
                checkpoint_prefix_recovery:
                    tilo_relay::CheckpointPrefixRecovery::NativeOwnerDurableBaseline,
            }],
            ..Default::default()
        },
        tilo_relay::daemon::RelayIdentity {
            user_public_key: governor.user_public_key,
            device_signing_key: host_key,
            device_cert: host.clone(),
            revocations: RevocationList::new(),
        },
        server.clone(),
        vec![(
            WORKSPACE_JOIN_ALPN.to_vec(),
            Box::new(WorkspaceJoinProtocol::new(JoinService(Arc::clone(
                &authority,
            )))),
        )],
    )
    .await
    .unwrap();

    let custody = HostIdentity {
        device_cert: device.clone(),
        device_signing_key: device_key.clone(),
        workspace_device_certs: std::collections::HashMap::default(),
        workspace_revocations: std::collections::HashMap::default(),
        identity_paths: None,
    };
    let own_room = RoomId::parse("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb").unwrap();
    let joined = Joined::open(
        directory.path(),
        custody,
        local_endpoint(&device_key).await,
        BTreeSet::from([own_room]),
        (
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::new(FanoutState::new()),
        ),
    )
    .unwrap();
    let invitation = join::encode_invitation_text(&issued.signed_invitation);
    assert_eq!(joined.join(&invitation).await.unwrap(), room);
    joined
        .owner
        .with_current(room, |view| {
            let (role, _, _) = view
                .peer()
                .permissions(device.device_public_key, SystemTime::now())?;
            assert_eq!(
                role,
                collab_types::Role::Editor,
                "a foreign device joins as an editor"
            );
            Ok(())
        })
        .unwrap();
    assert!(
        joined
            .workspaces()
            .iter()
            .any(|workspace| workspace.room == room),
        "the joined room is a workspace of this profile"
    );
    assert!(
        std::fs::read(directory.path().join(JOINED_ROOMS)).is_ok(),
        "the enrollment is kept in the profile's own file"
    );

    // The governor publishes the room's history through its host; the
    // runtime's own session admits it.
    let (access, publications) = {
        let authority = authority.lock().unwrap();
        let owner = &authority.0;
        (
            owner.peer().governed_access(SystemTime::now()).unwrap(),
            owner
                .outgoing()
                .iter()
                .map(|entry| (entry.bytes.to_vec(), entry.kind))
                .collect::<Vec<_>>(),
        )
    };
    relay
        .replace_governed_workspace_access(topic, true, access, relay_root)
        .await
        .unwrap();
    let publisher_endpoint = local_endpoint(&governor_key).await;
    let client = tilo_relay::RelayClient::connect_v2_to_enrolled_home_peer(
        publisher_endpoint.clone(),
        server.addr(),
        host.user_public_key,
        host.device_public_key,
        governor.clone(),
        governor_key,
        RevocationList::new(),
    )
    .await
    .unwrap();
    let publisher = RelayCoeditSession::join(client, topic, governor.device_public_key).unwrap();
    tilo_test_env::within("the governor's session", async {
        while !matches!(publisher.recv().await, Some(Inbound::Ready { .. })) {}
    })
    .await;
    tilo_test_env::within("the runtime's session", async {
        while !relay.is_subscribed(topic, &device.device_public_key).await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    for (bytes, kind) in &publications {
        let message: NeighbourMsg = postcard::from_bytes(bytes).unwrap();
        let checkpoint = *kind == tilo_room::transport::Retention::Checkpoint;
        let (channel, _) = tilo_room::transport::retention(&message, checkpoint).unwrap();
        if checkpoint {
            publisher.publish_checkpoint(channel, bytes.clone())
        } else {
            publisher.publish_retained_delta(channel, bytes.clone())
        }
        .unwrap();
    }
    let chat = tilo_test_env::within("the governor's history", async {
        loop {
            if let Ok(chat) = joined.owner.with_current(room, |view| {
                view.peer()
                    .document("so.tilo.chat", "")
                    .ok_or_else(|| "not yet".to_owned())
            }) {
                break chat;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;

    // A local write commits durably and reaches the governor.
    let mut doc = AutoCommit::load(&chat).unwrap();
    let heads = doc.get_heads();
    doc.put(ROOT, "topic", "written in the runtime").unwrap();
    doc.commit();
    let message = joined
        .owner
        .with_current(room, |view| {
            view.peer().clone().publish_incremental(
                "so.tilo.chat",
                "",
                doc.save_after(&heads),
                &device_key,
                SystemTime::now(),
            )
        })
        .unwrap();
    let NeighbourMsg::AppDocument { envelope, .. } = message else {
        panic!("a signed document write")
    };
    let receipt = joined
        .commit(OwnerCommitRequest {
            room: room.to_string(),
            operation: CommitOperation::Document {
                app: "so.tilo.chat".into(),
                record: String::new(),
                envelope,
            },
        })
        .await;
    receipt.expect("a member's write commits to the joined room");
    tilo_test_env::within("the write at the governor", async {
        loop {
            if let Some(Inbound::Delta { author, .. }) = publisher.recv().await
                && author == device.device_public_key
            {
                break;
            }
        }
    })
    .await;

    // A sealed write, as an SDK app seals it under a key only the room's
    // key holders have: this member's owner and the host keep only its
    // ciphertext, and the governor receives it sealed.
    let channel = collab_types::document_channel_id("so.tilo.chat", &room.to_string(), "");
    let content = channel_crypto::ChannelGroup::create(
        channel,
        channel_crypto::MemberKey::generate("steward").unwrap(),
    )
    .unwrap()
    .content_keys()
    .unwrap();
    let mut doc = AutoCommit::load(&chat).unwrap();
    let heads = doc.get_heads();
    doc.put(ROOT, "secret", SECRET).unwrap();
    doc.commit();
    let message = joined
        .owner
        .with_current(room, |view| {
            view.peer().clone().publish_incremental(
                "so.tilo.chat",
                "",
                doc.save_after(&heads),
                &device_key,
                SystemTime::now(),
            )
        })
        .unwrap();
    let NeighbourMsg::AppDocument { mut envelope, .. } = message else {
        panic!("a signed document write")
    };
    let (metadata, sealed) = content
        .seal_content(
            envelope.artifact,
            collab_types::ContentRetention::Delta,
            &envelope.payload,
        )
        .unwrap();
    envelope.payload = sealed;
    envelope.encryption = Some(metadata);
    collab_types::sign_op(&mut envelope, &device_key).unwrap();
    joined
        .commit(OwnerCommitRequest {
            room: room.to_string(),
            operation: CommitOperation::Document {
                app: "so.tilo.chat".into(),
                record: String::new(),
                envelope,
            },
        })
        .await
        .expect("a keyless member keeps a sealed write as ciphertext");
    tilo_test_env::within("the sealed write at the governor", async {
        loop {
            if let Some(Inbound::Delta { author, data }) = publisher.recv().await
                && author == device.device_public_key
                && let Ok(NeighbourMsg::AppDocument { envelope, .. }) = postcard::from_bytes(&data)
                && envelope.encryption.is_some()
            {
                break;
            }
        }
    })
    .await;
    for file in files_under(directory.path()) {
        let bytes = std::fs::read(&file).unwrap();
        assert!(
            !bytes
                .windows(SECRET.len())
                .any(|window| window == SECRET.as_bytes()),
            "{} holds the plaintext",
            file.display()
        );
    }
    publisher.close().await;
    publisher_endpoint.close().await;
    relay.shutdown().await;
}

/// Every file under `root`: the host's relay store, the runtime's record.
fn files_under(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    assert!(!files.is_empty());
    files
}

#[test]
fn the_rooms_the_desktop_session_enrolled_are_refused() {
    let directory = tempfile::tempdir().unwrap();
    assert!(desktop_enrolled(directory.path()).unwrap().is_empty());
    std::fs::write(
        directory
            .path()
            .join(super::super::workspace::DESKTOP_WORKSPACE),
        br#"{"native_authority": {"revision": 1, "rooms": {}, "pending": {}}}"#,
    )
    .unwrap();
    assert!(desktop_enrolled(directory.path()).unwrap().is_empty());
    std::fs::write(
        directory
            .path()
            .join(super::super::workspace::DESKTOP_WORKSPACE),
        b"not a workspace file",
    )
    .unwrap();
    assert!(
        desktop_enrolled(directory.path()).is_err(),
        "an unreadable desktop record refuses rather than guesses"
    );
}
