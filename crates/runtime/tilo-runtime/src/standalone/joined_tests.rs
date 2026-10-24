//! Joining a room a certified host serves, as a foreign device.
#![allow(
    clippy::significant_drop_tightening,
    clippy::too_many_lines,
    reason = "one linear join scenario over a live host; its fixtures hold their locks and \
              endpoints for the whole test"
)]
use super::*;
use crate::RuntimeServices as _;
use automerge::{AutoCommit, ROOT, transaction::Transactable};
use collab_types::CommitNamed as _;
use ed25519_dalek::SigningKey;
use std::sync::Arc;
use tilo_action_broker::channel_encryption::{
    ChannelAccess, ChannelDirectory, ChannelEncryption, ChannelEncryptionConfig, ChannelKeyAccess,
    GovernanceTransport, InboundChannelGovernance,
};
use tilo_delivery_protocol::workspace_join::{WORKSPACE_JOIN_ALPN, WorkspaceJoinService};
use tilo_joined_room::testing::governor::TestGovernor;
use tilo_owner_commit::CommitOperation;
use tilo_peer_state::{
    StatePeer,
    join::{self, JoinTransportRequest, JoinTransportResponse, MemberRole},
};
use tilo_relay::workspace_join::WorkspaceJoinProtocol;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

type Authority = Arc<std::sync::Mutex<TestGovernor>>;

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
        self.0.lock().unwrap().handle(remote, request)
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

impl Joined {
    pub(in crate::standalone) fn test_certificate(&self) -> &DeviceCert {
        &self.custody.device_cert
    }

    pub(in crate::standalone) async fn test_empty() -> (Arc<Self>, tempfile::TempDir) {
        let directory = tempfile::tempdir().unwrap();
        let key = SigningKey::from_bytes(&[93; 32]);
        let root = SigningKey::from_bytes(&[94; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "Synthetic runtime".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();
        let joined = Self::open(
            directory.path(),
            HostIdentity {
                device_cert: cert,
                device_signing_key: key.clone(),
                workspace_device_certs: std::collections::HashMap::default(),
                workspace_revocations: std::collections::HashMap::default(),
                identity_paths: None,
            },
            local_endpoint(&key).await,
            BTreeSet::new(),
            (
                Arc::new(Mutex::new(BrokerChannels::default())),
                Arc::new(FanoutState::new()),
            ),
        )
        .unwrap();
        (joined, directory)
    }
}

/// A room its governor's certified host serves over a relay with the join
/// service, and the governor's own session on it once it publishes.
struct HostedRoom {
    authority: Authority,
    relay: tilo_relay::Relay,
    server: iroh::Endpoint,
    relay_root: std::path::PathBuf,
    room: RoomId,
    topic: u64,
    governor: DeviceCert,
    governor_key: SigningKey,
    host: DeviceCert,
    hint: join::TransportHint,
}

impl HostedRoom {
    async fn start(dir: &std::path::Path) -> Self {
        let governing = TestGovernor::new();
        let governor = governing.governor().clone();
        let governor_key = governing.governor_key().clone();
        let host = governing.host().clone();
        let host_key = governing.host_key().clone();
        let server = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .bind()
            .await
            .unwrap();
        let room = RoomId::parse(governing.room()).unwrap();
        let topic = room_topic(room).unwrap();
        let hint = governing.hint(server.addr());
        let relay_root = dir.join("relay");
        let baseline = governing.peer().admitted_room().unwrap().clone().save();
        tilo_relay::store::WorkspaceStore::open(&relay_root, topic, true)
            .unwrap()
            .save_snapshot(&baseline)
            .unwrap();
        let authority: Authority = Arc::new(std::sync::Mutex::new(governing));
        let relay = tilo_relay::Relay::start_on_endpoint_with_protocols(
            tilo_relay::RelayConfig {
                state_root: relay_root.clone(),
                identity_root: dir.join("relay-identity"),
                full_replica_status: dir.join("replica/status.json"),
                home_peer_bundle: dir.join("home-peer.toml"),
                trust_policy: tilo_relay::TrustPolicy::AllowList {
                    users: vec![hex::encode(governor.user_public_key)],
                },
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
        Self {
            authority,
            relay,
            server,
            relay_root,
            room,
            topic,
            governor,
            governor_key,
            host,
            hint,
        }
    }

    /// An editor's invitation for a device of `recipient`.
    fn invite(&self, recipient: [u8; 32]) -> String {
        let now = ns(SystemTime::now());
        let signed = self.authority.lock().unwrap().issue(
            vec![self.hint.clone()],
            MemberRole::Editor,
            Some(recipient),
            Some("Runtime".into()),
            now.saturating_add(60_000_000_000),
        );
        join::encode_invitation_text(&signed)
    }

    /// Admit the room's current members at the relay, open the governor's
    /// session once `member` is subscribed, and publish the room's history
    /// through it.
    async fn publish_history(&self, member: [u8; 32]) -> (RelayCoeditSession, iroh::Endpoint) {
        let (access, publications) = {
            let owner = self.authority.lock().unwrap();
            (
                owner.governed_access(SystemTime::now()).unwrap(),
                owner
                    .outgoing()
                    .map(|(bytes, kind)| (bytes.to_vec(), kind))
                    .collect::<Vec<_>>(),
            )
        };
        self.relay
            .replace_governed_workspace_access(self.topic, true, access, self.relay_root.clone())
            .await
            .unwrap();
        let endpoint = local_endpoint(&self.governor_key).await;
        let client = tilo_relay::RelayClient::connect_v2_to_enrolled_home_peer(
            endpoint.clone(),
            self.server.addr(),
            self.host.user_public_key,
            self.host.device_public_key,
            self.governor.clone(),
            self.governor_key.clone(),
            RevocationList::new(),
        )
        .await
        .unwrap();
        let publisher =
            RelayCoeditSession::join(client, self.topic, self.governor.device_public_key).unwrap();
        tilo_test_env::within("the governor's session", async {
            while !matches!(publisher.recv().await, Some(Inbound::Ready { .. })) {}
        })
        .await;
        tilo_test_env::within("the member's session", async {
            while !self.relay.is_subscribed(self.topic, &member).await {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        for (bytes, kind) in &publications {
            let message: NeighbourMsg = postcard::from_bytes(bytes).unwrap();
            let checkpoint = *kind == neighbour_wire::Retention::Checkpoint;
            let (channel, _) = neighbour_wire::retention(&message, checkpoint).unwrap();
            if checkpoint {
                publisher.publish_checkpoint(channel, bytes.clone())
            } else {
                publisher.publish_retained_delta(channel, bytes.clone())
            }
            .unwrap();
        }
        (publisher, endpoint)
    }
}

fn assert_changed_custody_refused(joined: &Joined, room: RoomId, actor: &str) {
    let mut custody = joined.custody.clone();
    let room_cert = custody.device_cert.clone();
    let default_root = SigningKey::from_bytes(&[95; 32]);
    custody.device_cert.user_public_key = default_root.verifying_key().to_bytes();
    sign_device_cert(&mut custody.device_cert, &default_root).unwrap();
    let default_actor = tilo_action_broker::collab_broker_capnp::principal_id_for_root(
        custody.device_cert.user_public_key,
    );
    custody
        .workspace_device_certs
        .insert(room_topic(room).unwrap(), room_cert);
    let mut probe = Joined {
        executions: super::super::execution::ExecutionPins::new(&joined.owner),
        owner: joined.owner.clone(),
        custody,
        endpoint: joined.endpoint.clone(),
        refused: joined.refused.clone(),
        channels: Arc::clone(&joined.channels),
        fanout: Arc::clone(&joined.fanout),
        sessions: Mutex::new(BTreeMap::new()),
        governance: Mutex::new(Vec::new()),
        governance_wake: joined.governance_wake(),
        keys: joined.keys.clone(),
        content: std::sync::OnceLock::new(),
        requesters: content::Requesters::default(),
    };
    assert!(
        probe.action_room(actor, &room.to_string()).is_ok(),
        "the admitted selected root is independent of the default root"
    );
    assert!(
        probe
            .action_room(&default_actor, &room.to_string())
            .is_err(),
        "a default root must not borrow the selected root's admission"
    );
    let run = probe
        .admit_mcp_execution(&room.to_string(), "different-own-room", actor)
        .unwrap();
    let retained = run.caller().clone();
    probe.recheck_mcp_executions(room).unwrap();
    assert!(retained.execution_is_live());
    let original = probe.custody.workspace_device_certs.clone();
    probe.custody.workspace_device_certs.clear();
    assert!(probe.recheck_mcp_executions(room).is_err());
    assert!(
        !retained.execution_is_live(),
        "authority-side original-pin recheck closes retained custody without a wrapper call"
    );
    probe.custody.workspace_device_certs = original;
    probe.recheck_mcp_executions(room).unwrap();
    assert!(
        !retained.execution_is_live(),
        "restoring original selection cannot revive the retired run"
    );
    probe.custody.workspace_device_certs.clear();
    assert!(
        probe.action_room(actor, &room.to_string()).is_err(),
        "changed selected custody must not reuse the retained roster identity"
    );
    assert!(
        !probe
            .workspaces()
            .iter()
            .any(|workspace| workspace.room() == room)
    );
}

fn assert_runtime_admission_revoked(joined: &Arc<Joined>, room: RoomId, actor: &str) {
    use crate::standalone::RuntimeActionHost;
    use tilo_action_broker::{
        collab_broker::CrdtRoute,
        collab_broker_capnp::{
            CapnpBrokerState, PublishedAction, PublishedInput, PublishedSurface,
        },
        test_support::{Workspace, WorkspaceId, WorkspaceTree},
    };
    use tilo_actions::remote::{
        ActionHost, ActionHostError, Caller, CatalogueQuery, InvokeRequest,
    };
    use tilo_actions::remote::{InvokeArgument, PendingPoll, RemoteActionTarget};

    let mut workspace = Workspace::in_room(WorkspaceId(41), room, "Execution isolation");
    workspace.set_owner(actor.to_owned());
    let room = room.to_string();
    let directory = tempfile::tempdir().unwrap();
    let mut tree = WorkspaceTree {
        persist_path: Some(directory.path().join("workspace.json")),
        ..WorkspaceTree::default()
    };
    tree.desktop.workspaces.push(workspace);
    let membership = tree.shared();
    let origin = tilo_action_broker::peer_actions::OwnerPolicyClock::new()
        .unwrap()
        .new_origin();
    let admission = joined
        .admit_mcp_execution(&room, "private-profile-room", actor)
        .unwrap();
    let mut services = membership.services();
    services.native_certificates = Some(Arc::new(super::admission_tests::FixtureAuthority {
        room: uuid::Uuid::parse_str(&room).unwrap(),
        cert: admission.certificate().clone(),
        time: origin.capture().unwrap(),
    }));
    let (broker, turn) = CapnpBrokerState::test_broker_with_authority_and_invoke_turn(
        membership.authority(),
        services,
    );
    let qualified = broker
        .certified_local_action_host(admission.caller(), admission.presentation())
        .unwrap();
    broker.replace_published_surfaces_for_test(vec![PublishedSurface {
        peer_id: 7,
        app_id: "so.tilo.files".into(),
        route: CrdtRoute::app("so.tilo.files", room.clone()),
        actions: vec![PublishedAction {
            semantic_identity: None,
            resource: "files.entry:/tmp/synthetic-report".into(),
            action: "refresh".into(),
            resource_kind: "files.entry".into(),
            resource_label: "Synthetic report".into(),
            action_label: "Refresh".into(),
            description: None,
            operation_kind: tilo_actions::OperationKind::Effect,
            params: Vec::new(),
            contract: tilo_actions::ActionContract {
                execution: tilo_actions::ExecutionMode::Background { cancellable: true },
                result: tilo_actions::ResultSpec::Job,
                ..Default::default()
            },
            input: PublishedInput::None,
            view: None,
        }],
        live: true,
    }]);
    let mut invocations = broker.capture_invocations_for_test(7);
    let owner = Caller::local_person(actor).in_room(room.clone());
    let host = RuntimeActionHost {
        broker,
        room: "private-profile-room".into(),
        joined: Arc::clone(joined),
        session: Some(Arc::new(crate::standalone::AdmittedRuntimeSession {
            policy_time: origin.capture().unwrap(),
            host: qualified,
            admission,
            agent: None,
            projection: Some(owner.clone()),
        })),
    };
    let id = tilo_actions::InvocationId::new();
    let request = InvokeRequest {
        on_behalf_of: None,
        invocation: id,
        target: RemoteActionTarget {
            provider: "7".into(),
            address: tilo_actions::ActionAddress::new(
                "files.entry:/tmp/synthetic-report",
                "refresh",
            ),
        },
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
    };
    let mut pending = host.invoke(&owner, request.clone());
    assert!(matches!(pending.poll_ready(), PendingPoll::Waiting));
    turn();
    let outcome = pending.poll_ready();
    let PendingPoll::Ready(Ok(_receipt)) = outcome else {
        panic!("the synthetic app must accept the invocation: {outcome:?}");
    };
    assert!(invocations.try_recv().is_ok());
    assert!(host.observe(&owner, &id).is_some());
    joined
        .owner
        .invalidate_identity(RoomId::parse(&room).unwrap());
    assert!(host.observe(&owner, &id).is_none());
    assert_eq!(host.cancel(&owner, &id), Err(ActionHostError::NotPermitted));
    assert!(matches!(
        host.invoke(&owner, request).poll_ready(),
        PendingPoll::Ready(Err(ActionHostError::NotPermitted))
    ));
    assert!(matches!(
        host.query(&owner, CatalogueQuery::default()).poll_ready(),
        PendingPoll::Ready(Err(ActionHostError::NotPermitted))
    ));
    assert!(
        invocations.try_recv().is_err(),
        "revoked retries do not dispatch"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_foreign_runtime_joins_a_hosted_room_as_an_editor_and_syncs_both_ways() {
    if crate::standalone::tests::isolated_signing_identity("standalone::joined::tests::a_foreign_runtime_joins_a_hosted_room_as_an_editor_and_syncs_both_ways").is_none() {
        return;
    }
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let governing = Box::pin(HostedRoom::start(directory.path())).await;
    let room = governing.room;

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
    let invitation = governing.invite(device.user_public_key);

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
    let actor =
        tilo_action_broker::collab_broker_capnp::principal_id_for_root(device.user_public_key);
    assert!(
        joined.action_room(&actor, &room.to_string()).is_err(),
        "an invitation is not current admission"
    );
    assert_eq!(joined.join(&invitation).await.unwrap(), room);
    assert_eq!(
        joined.action_room(&actor, &room.to_string()).unwrap(),
        room.to_string()
    );
    assert!(
        joined
            .action_room("forged-profile-actor", &room.to_string())
            .is_err()
    );
    assert!(joined.action_room(&actor, &own_room.to_string()).is_err());
    assert!(joined.action_room(&actor, "not-a-room").is_err());
    let execution = joined
        .admit_mcp_execution(&room.to_string(), &own_room.to_string(), &actor)
        .unwrap();
    let presentation = runtime_collab::CertifiedJoinIdentity::decode(execution.presentation())
        .unwrap()
        .unwrap();
    assert_eq!(presentation.workspace_id, room.to_string());
    let presented: DeviceCert = serde_json::from_slice(&presentation.device_certificate).unwrap();
    assert_eq!(presented, device);
    device_key
        .verifying_key()
        .verify_strict(
            &runtime_identity::collaboration_join_signing_bytes(&room.to_string()),
            &ed25519_dalek::Signature::from_slice(&presentation.proof).unwrap(),
        )
        .unwrap();
    let retained_caller = execution.caller().clone();
    {
        use tilo_action_broker::peer_actions::CertificateAuthority as _;
        let origin = tilo_action_broker::peer_actions::OwnerPolicyClock::new()
            .unwrap()
            .new_origin();
        let authority = super::super::Certificates {
            room: own_room.to_string(),
            device: device.device_public_key,
            policy_time: origin.capture().unwrap(),
            joined: Arc::downgrade(&joined),
        };
        let retained_time = authority.policy_time().unwrap().unwrap();
        let mut forged = device.clone();
        forged.device_label =
            "attacker-supplied certificate claiming the public local device".into();
        assert!(
            authority
                .authorize_join(uuid::Uuid::parse_str(&room.to_string()).unwrap(), &forged)
                .is_err()
        );
        assert!(
            retained_time.now_ns().is_ok(),
            "unverified input cannot retire the runtime policy origin"
        );
        assert!(
            retained_caller.execution_is_live(),
            "unverified input cannot retire an independently pinned real run"
        );
        joined.recheck_mcp_executions(room).unwrap();
    }

    assert_eq!(
        retained_caller.user_public_key(),
        Some(&device.user_public_key)
    );
    assert_eq!(
        retained_caller.device_public_key(),
        Some(&device.device_public_key)
    );
    assert!(
        joined
            .admit_mcp_execution(
                &room.to_string(),
                &own_room.to_string(),
                "forged-profile-actor"
            )
            .is_err()
    );
    joined
        .owner
        .with_current(room, |view| {
            let role = view
                .peer()
                .permissions(device.device_public_key, SystemTime::now())?
                .role;
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
            .any(|workspace| workspace.room() == room),
        "the joined room is a workspace of this profile"
    );
    assert!(
        std::fs::read(directory.path().join(JOINED_ROOMS)).is_ok(),
        "the enrollment is kept in the profile's own file"
    );

    // The governor publishes the room's history through its host; the
    // runtime's own session admits it.
    let (publisher, publisher_endpoint) = governing.publish_history(device.device_public_key).await;
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
    doc.commit_edit("fixture", "topic");
    let message = joined
        .owner
        .with_current(room, |view| {
            view.peer().clone().publication_mut().publish_incremental(
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
    doc.commit_edit("fixture", "secret");
    let message = joined
        .owner
        .with_current(room, |view| {
            view.peer().clone().publication_mut().publish_incremental(
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
    assert_changed_custody_refused(&joined, room, &actor);
    assert_runtime_admission_revoked(&joined, room, &actor);
    assert!(
        !retained_caller.execution_is_live(),
        "native enrollment fence synchronously closes captured run custody"
    );
    assert!(execution.check(&joined).is_err());
    assert!(
        joined
            .admit_mcp_execution(&room.to_string(), &own_room.to_string(), &actor)
            .is_err()
    );
    assert!(
        !joined
            .workspaces()
            .iter()
            .any(|workspace| workspace.room() == room),
        "broker snapshots must drop stale admission too"
    );
    assert!(
        joined.action_room(&actor, &room.to_string()).is_err(),
        "stale identity removes the selected audience"
    );
    publisher.close().await;
    publisher_endpoint.close().await;
    governing.relay.shutdown().await;
    for (file, bytes) in settled_files(directory.path()) {
        assert!(
            !bytes
                .windows(SECRET.len())
                .any(|window| window == SECRET.as_bytes()),
            "{} holds the plaintext",
            file.display()
        );
    }
}

const SDK_TEST: &str = "standalone::joined::tests::a_live_sdk_app_in_a_joined_room_gets_keys_from_the_governors_steward";
const SDK_HELPER: &str = "TILO_JOINED_SDK_HELPER";
const SDK_APP: &str = "so.tilo.chat";
const SDK_WRITTEN: &str = "sealed by a live SDK app in a joined room";

#[test]
fn a_live_sdk_app_in_a_joined_room_gets_keys_from_the_governors_steward() {
    // Helper role: an ordinary SDK app in the joined room writes once. The
    // SDK holds the write until the runtime's channel service is admitted by
    // the room's steward, then seals and sends it.
    if std::env::var_os(SDK_HELPER).is_some() {
        let mut manager = ui::collaboration::CollaborationManager::new();
        let started = std::time::Instant::now();
        while !manager.is_connected() && started.elapsed() < tilo_test_env::hang_bound() {
            let _ = manager.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(manager.is_connected(), "{:?}", manager.state());
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "text", SDK_WRITTEN).unwrap();
        doc.commit_edit("fixture", "text");
        assert!(
            manager.send(ui::collaboration::CollabRequest::document_snapshot(
                doc.save()
            ))
        );
        loop {
            let _ = manager.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let Some(profile) = super::super::tests::isolated_root(SDK_TEST) else {
        return;
    };
    let session = super::super::Standalone::start_reporting(&profile, &mut |_| {}).unwrap();
    let (_, device) = tilo_identity_host::broker::trusted_current_device(session.room()).unwrap();
    let governor_root = profile.parent().unwrap().join("governor");
    std::fs::create_dir_all(&governor_root).unwrap();
    let governing = tilo_rt::app()
        .block_on(Box::pin(HostedRoom::start(&governor_root)))
        .unwrap();
    let room = session.joiner().unwrap()(&governing.invite(device.user_public_key)).unwrap();
    assert_eq!(room, governing.room.to_string());
    let (publisher, publisher_endpoint) = tilo_rt::app()
        .block_on(governing.publish_history(device.device_public_key))
        .unwrap();
    let steward = Steward::open(&governor_root, &governing.governor_key, governing.room);
    // A member writes only into a record it holds, and joining brings the
    // room's governance alone: the chat record arrives through the room's
    // sync, once the host has kept the governor's history. The app opens
    // in the room as it stands then.
    tilo_rt::app()
        .block_on(tilo_test_env::within("the governor's history", async {
            while session
                .joined
                .owner
                .with_current(governing.room, |view| {
                    view.peer()
                        .document(SDK_APP, "")
                        .map(drop)
                        .ok_or_else(|| "not yet".to_owned())
                })
                .is_err()
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }))
        .unwrap();

    let log = std::fs::File::create(profile.join("sdk-app.log")).unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", SDK_TEST, "--nocapture"])
        .env(SDK_HELPER, "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log.try_clone().unwrap()))
        .stderr(std::process::Stdio::from(log));
    let mut app = crate::unix::launch(
        command,
        SDK_APP,
        &room,
        &crate::unix::ServiceSockets {
            collab: session.broker.capnp_socket(),
            identity: &session.identity_socket,
            blobs: &session.blob_socket,
        },
    )
    .unwrap();

    // The governor's steward answers the runtime's access request over the
    // room's relay session; the app's write then arrives sealed.
    let sealed = tilo_rt::app().block_on(tilo_test_env::within(
        "the SDK app's sealed write at the governor",
        async {
            let mut inbound = Vec::new();
            let mut tick = tokio::time::interval(Duration::from_millis(100));
            loop {
                tokio::select! {
                    received = publisher.recv() => match received.expect("the governor's session") {
                        Inbound::Presence { payload, .. } => inbound.extend(
                            postcard::from_bytes(&payload)
                                .ok()
                                .and_then(InboundChannelGovernance::from_message),
                        ),
                        Inbound::Delta { author, data } if author == device.device_public_key => {
                            if let Ok(NeighbourMsg::AppDocument { envelope, .. }) = postcard::from_bytes(&data)
                                && envelope.encryption.is_some()
                            {
                                break *envelope;
                            }
                        }
                        _ => {}
                    },
                    _ = tick.tick() => {
                        let authority = governing.authority.lock().unwrap();
                        steward.pump(
                            authority.peer(),
                            &RelayGovernance {
                                session: &publisher,
                                inbound: std::sync::Mutex::new(std::mem::take(&mut inbound)),
                            },
                        );
                    }
                }
            }
        },
    ))
    .unwrap();
    let _ = app.kill();
    let _ = app.wait();

    // A key holder reads it: the steward opens what the app sealed.
    let metadata = sealed.encryption.unwrap();
    let (ChannelAccess::Ready(keys), _) = steward.keys.access(governing.room, metadata.channel)
    else {
        panic!("the steward holds the channel's keys")
    };
    let plaintext = keys
        .open_content(sealed.artifact, &metadata, &sealed.payload)
        .unwrap();
    let collab_types::DocumentMutation::Snapshot(bytes) =
        collab_types::DocumentMutation::decode(&plaintext).unwrap()
    else {
        panic!("the app sent its document as a snapshot")
    };
    let doc = AutoCommit::load(&bytes).unwrap();
    let text = automerge::ReadDoc::get(&doc, ROOT, "text")
        .unwrap()
        .and_then(|(value, _)| value.into_string().ok());
    assert_eq!(text.as_deref(), Some(SDK_WRITTEN));
    tilo_rt::app()
        .block_on(async {
            publisher.close().await;
            publisher_endpoint.close().await;
            governing.relay.shutdown().await;
        })
        .unwrap();
    // Neither the runtime's profile nor the host keeps it in the clear.
    for root in [&profile, &governor_root] {
        for (file, bytes) in settled_files(root) {
            assert!(
                !bytes
                    .windows(SDK_WRITTEN.len())
                    .any(|window| window == SDK_WRITTEN.as_bytes()),
                "{} holds the plaintext",
                file.display()
            );
        }
    }
}

/// The governor's steward of the room's encrypted channels: it admits every
/// certified, unrevoked device the room's governance lets read.
struct Steward {
    keys: ChannelKeyAccess,
    readers: Arc<std::sync::Mutex<BTreeSet<collab_types::Ed25519PublicKey>>>,
}

impl Steward {
    fn open(dir: &std::path::Path, device_key: &SigningKey, room: RoomId) -> Self {
        let readers = Arc::default();
        let service = ChannelEncryption::open(
            ChannelEncryptionConfig {
                root: dir.join("channels"),
                vault: channel_crypto::VaultKey::new(blake3::derive_key(
                    "tilo joined test steward v1",
                    &device_key.to_bytes(),
                )),
                device_key: device_key.clone(),
            },
            Box::new(StewardDirectory {
                room,
                readers: Arc::clone(&readers),
            }),
            Arc::new(Mutex::new(BrokerChannels::default())),
        )
        .unwrap();
        Self {
            keys: ChannelKeyAccess::new(service),
            readers,
        }
    }

    fn pump(&self, peer: &StatePeer, transport: &dyn GovernanceTransport) {
        let now = SystemTime::now();
        *self.readers.lock().unwrap() = peer
            .certificates()
            .into_iter()
            .filter(|cert| !cert.capabilities.can_act_as_host)
            .filter(|cert| peer.permissions(cert.device_public_key, now).is_ok())
            .map(|cert| collab_types::Ed25519PublicKey::from_bytes(cert.device_public_key))
            .collect();
        self.keys.pump(transport);
    }
}

struct StewardDirectory {
    room: RoomId,
    readers: Arc<std::sync::Mutex<BTreeSet<collab_types::Ed25519PublicKey>>>,
}

impl ChannelDirectory for StewardDirectory {
    fn governs(&self, room: RoomId) -> bool {
        room == self.room
    }

    fn steward_of(&self, _room: RoomId) -> Option<collab_types::Ed25519PublicKey> {
        None
    }

    fn allowed_devices(
        &self,
        room: RoomId,
        _channel: collab_types::ChannelId,
        _route: Option<&collab_types::ChannelRoute>,
        _now: u64,
    ) -> BTreeSet<collab_types::Ed25519PublicKey> {
        if room != self.room {
            return BTreeSet::new();
        }
        self.readers.lock().unwrap().clone()
    }
}

/// The governor's relay session as the steward's transport: governance goes
/// to every member as room presence.
struct RelayGovernance<'a> {
    session: &'a RelayCoeditSession,
    inbound: std::sync::Mutex<Vec<InboundChannelGovernance>>,
}

impl GovernanceTransport for RelayGovernance<'_> {
    fn publish(&self, message: NeighbourMsg) -> bool {
        postcard::to_allocvec(&message)
            .is_ok_and(|bytes| self.session.publish_presence(bytes).is_ok())
    }

    fn take_inbound(&self) -> Vec<InboundChannelGovernance> {
        std::mem::take(&mut *self.inbound.lock().unwrap())
    }
}

/// Every regular file under `root` with its contents: the host's relay store,
/// the runtime's record. A service socket holds no data.
///
/// The relay's detached store work and the joined session's record
/// replacement can outlive the services' shutdown, so an entry listed but
/// gone when read was renamed over or discarded: the scan starts again until
/// one pass reads every entry it lists.
fn settled_files(root: &std::path::Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    fn scan(root: &std::path::Path) -> std::io::Result<Vec<(std::path::PathBuf, Vec<u8>)>> {
        let mut files = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir)? {
                let path = entry?.path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.is_file() {
                    let bytes = std::fs::read(&path)?;
                    files.push((path, bytes));
                }
            }
        }
        Ok(files)
    }
    for _ in 0..100 {
        match scan(root) {
            Ok(files) => {
                assert!(!files.is_empty());
                return files;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("reading {}: {error}", root.display()),
        }
    }
    panic!("{} kept changing under every scan", root.display())
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

/// Content kept in memory, as a profile's content store keeps it.
#[derive(Default)]
struct Shelf(
    Mutex<
        std::collections::BTreeMap<
            (tilo_action_broker::record_content::ContentScope, String),
            Vec<u8>,
        >,
    >,
);

impl tilo_action_broker::record_content::ContentShelf for Shelf {
    fn read(
        &self,
        scope: &tilo_action_broker::record_content::ContentScope,
        hash: &str,
    ) -> Option<Vec<u8>> {
        self.0
            .lock()
            .ok()?
            .get(&(scope.clone(), hash.to_owned()))
            .cloned()
    }

    fn keep(
        &self,
        scope: &tilo_action_broker::record_content::ContentScope,
        hash: &str,
        bytes: &[u8],
    ) -> Result<(), String> {
        self.0
            .lock()
            .map_err(|_| "poisoned".to_owned())?
            .insert((scope.clone(), hash.to_owned()), bytes.to_vec());
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_joined_room_s_record_content_crosses_the_host_both_ways() {
    use tilo_action_broker::record_content::PIECE_BYTES;
    use tilo_content::hash_bytes;
    use tilo_peer_state::peer_handle;

    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let governing = Box::pin(HostedRoom::start(directory.path())).await;
    let room = governing.room;
    let device_key = SigningKey::from_bytes(&[95; 32]);
    let root_key = SigningKey::from_bytes(&[96; 32]);
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
    let invitation = governing.invite(device.user_public_key);
    let joined = Joined::open(
        directory.path(),
        HostIdentity {
            device_cert: device.clone(),
            device_signing_key: device_key.clone(),
            workspace_device_certs: std::collections::HashMap::default(),
            workspace_revocations: std::collections::HashMap::default(),
            identity_paths: None,
        },
        local_endpoint(&device_key).await,
        BTreeSet::new(),
        (
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::new(FanoutState::new()),
        ),
    )
    .unwrap();
    let content = content::record_content_on(&joined, Arc::new(Shelf::default()));
    assert_eq!(joined.join(&invitation).await.unwrap(), room);
    let (governor, _endpoint) = governing.publish_history(device.device_public_key).await;
    let governor_handle = peer_handle(&governing.governor.device_public_key);
    let runtime_handle = peer_handle(&device.device_public_key);
    let route =
        tilo_action_broker::route::CrdtRoute::record("so.tilo.slides", room.to_string(), "deck");

    // The runtime asks the room for a picture; the governor's device sends it.
    let picture: Vec<u8> = (0..PIECE_BYTES + 5)
        .map(|index| u8::try_from(index % 241).unwrap())
        .collect();
    let hash = hash_bytes(&picture);
    let mut arrivals = content.subscribe(&route);
    tilo_test_env::within("the runtime's request", async {
        loop {
            content.want(&route, &hash);
            let request = tokio::time::timeout(Duration::from_millis(500), async {
                loop {
                    if let Some(Inbound::Presence { author, payload }) = governor.recv().await
                        && author == device.device_public_key
                        && let Ok(NeighbourMsg::RecordContentWant {
                            requester,
                            hash: asked,
                            ..
                        }) = postcard::from_bytes(&payload)
                    {
                        break (requester, asked);
                    }
                }
            })
            .await;
            if let Ok((requester, asked)) = request {
                assert_eq!(requester, runtime_handle);
                assert_eq!(asked, hash);
                break;
            }
        }
    })
    .await;
    for (index, chunk) in picture.chunks(PIECE_BYTES).enumerate() {
        let piece = NeighbourMsg::RecordContentPiece {
            responder: governor_handle,
            requester: runtime_handle,
            app_id: route.app_id.clone(),
            cluster_id: route.cluster_id.clone(),
            hash: hash.clone(),
            total: picture.len() as u64,
            offset: (index * PIECE_BYTES) as u64,
            bytes: chunk.to_vec(),
        };
        governor
            .publish_record_content(
                device.device_public_key,
                postcard::to_allocvec(&piece).unwrap(),
            )
            .unwrap();
    }
    // Where the want stood comes first; then the picture.
    let arrival = tilo_test_env::within("the picture at the runtime", async {
        loop {
            let news = arrivals.recv().await.unwrap();
            if news.state == tilo_content::ContentState::Resolved {
                break news;
            }
        }
    })
    .await;
    assert_eq!(arrival.bytes, picture);

    // The governor's device asks for it in turn; the runtime answers it alone.
    let want = NeighbourMsg::RecordContentWant {
        requester: governor_handle,
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id.clone(),
        hash: hash.clone(),
    };
    governor
        .publish_presence(postcard::to_allocvec(&want).unwrap())
        .unwrap();
    let mut gathered = vec![0_u8; picture.len()];
    let mut received = 0;
    tilo_test_env::within("the picture at the governor", async {
        while received < picture.len() {
            if let Some(Inbound::RecordContent { from, frame }) = governor.recv().await {
                // The host, which keeps none of it, says so: the picture
                // comes from the runtime.
                if from == governing.host.device_public_key
                    && matches!(
                        postcard::from_bytes(&frame),
                        Ok(NeighbourMsg::RecordContentLacks { .. })
                    )
                {
                    continue;
                }
                assert_eq!(from, device.device_public_key);
                let Ok(NeighbourMsg::RecordContentPiece {
                    requester,
                    offset,
                    bytes,
                    ..
                }) = postcard::from_bytes(&frame)
                else {
                    panic!("a piece of record content");
                };
                assert_eq!(requester, governor_handle);
                let start = usize::try_from(offset).unwrap();
                gathered[start..start + bytes.len()].copy_from_slice(&bytes);
                received += bytes.len();
            }
        }
    })
    .await;
    assert_eq!(gathered, picture);
}

/// Two devices that reach each other only through the room's host: a want
/// whose holder is offline waits (the host keeps none of it), and the
/// moment the holder connects to the host the want is asked of it, and the
/// content arrives: waiting, then fetching, then here.
#[tokio::test(flavor = "multi_thread")]
async fn a_want_through_the_host_waits_for_its_offline_holder_and_arrives_when_it_connects() {
    use tilo_action_broker::record_content::PIECE_BYTES;
    use tilo_content::{ContentState, hash_bytes};
    use tilo_peer_state::peer_handle;

    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let governing = Box::pin(HostedRoom::start(directory.path())).await;
    let room = governing.room;
    let device_key = SigningKey::from_bytes(&[97; 32]);
    let root_key = SigningKey::from_bytes(&[98; 32]);
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
    let invitation = governing.invite(device.user_public_key);
    let joined = Joined::open(
        directory.path(),
        HostIdentity {
            device_cert: device.clone(),
            device_signing_key: device_key.clone(),
            workspace_device_certs: std::collections::HashMap::default(),
            workspace_revocations: std::collections::HashMap::default(),
            identity_paths: None,
        },
        local_endpoint(&device_key).await,
        BTreeSet::new(),
        (
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::new(FanoutState::new()),
        ),
    )
    .unwrap();
    let content = content::record_content_on(&joined, Arc::new(Shelf::default()));
    assert_eq!(joined.join(&invitation).await.unwrap(), room);
    // The holder publishes the room's history, then goes offline.
    drop(governing.publish_history(device.device_public_key).await);
    let governor_handle = peer_handle(&governing.governor.device_public_key);
    let runtime_handle = peer_handle(&device.device_public_key);
    let route =
        tilo_action_broker::route::CrdtRoute::record("so.tilo.ebook", room.to_string(), "library");
    let book: Vec<u8> = (0..PIECE_BYTES + 11)
        .map(|index| u8::try_from(index % 239).unwrap())
        .collect();
    let hash = hash_bytes(&book);
    let mut news = content.subscribe(&route);
    content.want(&route, &hash);

    // Asked of the host once connected, which keeps none of it: it waits.
    tilo_test_env::within("the want waits for a device that has it", async {
        let mut asked_the_host = false;
        loop {
            match news.recv().await.unwrap().state {
                ContentState::Pending => asked_the_host = true,
                ContentState::Waiting if asked_the_host => break,
                ContentState::Waiting => {}
                other => panic!("{other:?} while its only holder is offline"),
            }
        }
    })
    .await;

    // The holder connects to the host: the want is asked of it, and it
    // answers with the book.
    let (holder, _endpoint) = governing.publish_history(device.device_public_key).await;
    holder.live_join().unwrap();
    tilo_test_env::within("the want asked of the holder", async {
        loop {
            if let Some(Inbound::Presence { author, payload }) = holder.recv().await
                && author == device.device_public_key
                && let Ok(NeighbourMsg::RecordContentWant {
                    requester,
                    hash: asked,
                    ..
                }) = postcard::from_bytes(&payload)
            {
                assert_eq!((requester, asked.as_str()), (runtime_handle, hash.as_str()));
                break;
            }
        }
    })
    .await;
    assert_eq!(
        news.recv().await.unwrap().state,
        ContentState::Pending,
        "fetching from the holder"
    );
    for (index, chunk) in book.chunks(PIECE_BYTES).enumerate() {
        let piece = NeighbourMsg::RecordContentPiece {
            responder: governor_handle,
            requester: runtime_handle,
            app_id: route.app_id.clone(),
            cluster_id: route.cluster_id.clone(),
            hash: hash.clone(),
            total: book.len() as u64,
            offset: (index * PIECE_BYTES) as u64,
            bytes: chunk.to_vec(),
        };
        holder
            .publish_record_content(
                device.device_public_key,
                postcard::to_allocvec(&piece).unwrap(),
            )
            .unwrap();
    }
    let arrived = tilo_test_env::within("the book at the runtime", news.recv())
        .await
        .unwrap();
    assert_eq!(arrived.state, ContentState::Resolved, "here");
    assert_eq!(arrived.bytes, book);
}
