//! Joining a workspace a certified host serves, as a foreign device.
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
use iroh::protocol::DynProtocolHandler;
use std::{sync::Arc, time::Duration};
use tilo_delivery_protocol::workspace_join::{
    IrohWorkspaceHostHintV1, WORKSPACE_JOIN_ALPN, WorkspaceJoinService,
};
use tilo_peer_state::join::{self, JoinTransportRequest, JoinTransportResponse, MemberRole};
use tilo_peer_state::owner::testing::governor::TestGovernor;
use tilo_peer_state::owner_commit::CommitOperation;
use tilo_relay::workspace_join::WorkspaceJoinProtocol;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

type Authority = Arc<std::sync::Mutex<TestGovernor>>;

/// What a sealed write says, which no file outside its key holders may hold.
const SECRET: &str = "only the workspace's key holders read this";

/// The workspace's host on the fixture's endpoint, as the governor's copy of
/// it: the certified host device admits the members the governor admitted,
/// carries each member's frames on to the others, and keeps none of their
/// content.
#[derive(Clone)]
struct HostParts {
    authority: Authority,
    host: DeviceCert,
    host_key: SigningKey,
    serving: tilo_peer_state::direct::Serving,
}

impl HostParts {
    /// Serve the workspace as the governor holds it now: the members a join
    /// just admitted are admitted here from then on.
    fn serve(&self) {
        let peer = self.authority.lock().unwrap().peer().clone();
        let link = DirectSync::over(
            tilo_peer_state::direct::Replica::new(peer),
            self.host_key.clone(),
        );
        self.serving.serve(link.clone()).unwrap();
        let host = tilo_peer_state::peer_handle(&self.host.device_public_key);
        let mut heard = link.heard();
        tilo_rt::app().spawn(async move {
            while let Ok(frame) = heard.recv().await {
                let NeighbourMsg::RecordContentWant {
                    requester,
                    app_id,
                    cluster_id,
                    hash,
                } = frame.message
                else {
                    continue;
                };
                let lacks = NeighbourMsg::RecordContentLacks {
                    responder: host,
                    requester,
                    app_id,
                    cluster_id,
                    hash,
                };
                let _ = link.send(lacks);
            }
        });
    }
}

#[derive(Clone)]
struct JoinService(HostParts);
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
        let response = self.0.authority.lock().unwrap().handle(remote, request);
        // The governor committed the join: the host serves the workspace as it
        // stands now, the newcomer admitted with it.
        self.0.serve();
        response
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
            issuer: None,
        };
        sign_device_cert(&mut cert, &root).unwrap();
        let joined = Self::open(
            directory.path(),
            HostIdentity {
                device_cert: cert,
                device_signing_key: key.clone(),
                workspace_device_certs: std::collections::HashMap::default(),
                workspace_revocations: std::collections::HashMap::default(),
                custody: None,
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

    /// [`Self::test_empty`] running its own workspace host, with `workspace` placed
    /// on it and held: admitted as every joined workspace is, its session not
    /// held yet.
    pub(in crate::standalone) async fn test_hosting(
        workspace: WorkspaceId,
    ) -> (Arc<Self>, tempfile::TempDir) {
        let (joined, directory) = Self::test_empty().await;
        let host = tilo_room_host::host::WorkspaceHost::open(
            tilo_room_host::host::HostConfig::new(
                directory.path().join("room-host"),
                tilo_room_host::host::HostMode::Device,
            ),
            tilo_room_host::host::HostIdentity {
                user_public_key: joined.custody.device_cert.user_public_key,
                device_signing_key: joined.custody.device_signing_key.clone(),
                device_cert: joined.custody.device_cert.clone(),
                revocations: RevocationList::new(),
            },
            joined.endpoint(),
            None,
        )
        .await
        .unwrap();
        joined
            .install_host(
                Arc::new(host),
                super::RecordCreation {
                    declared_kinds: Box::new(|app| {
                        (app == "dev.tilo.declared").then(|| vec!["dev.tilo.note".to_owned()])
                    }),
                    dev: false,
                },
            )
            .unwrap();
        joined.place(workspace, "Home").await.unwrap();
        (joined, directory)
    }
}

/// A workspace its governor's certified host serves over an endpoint with the
/// join service and its direct links, and the governor's own session on it
/// once it publishes.
struct HostedWorkspace {
    authority: Authority,
    relay: tilo_relay::Relay,
    workspace: WorkspaceId,
    governor: DeviceCert,
    governor_key: SigningKey,
    hint: join::TransportHint,
}

impl HostedWorkspace {
    async fn start(dir: &std::path::Path) -> Self {
        let governing = TestGovernor::new();
        let governor = governing.workspace_host().clone();
        let governor_key = governing.governor_key().clone();
        let host = governing.host().clone();
        let host_key = governing.host_key().clone();
        // The host's endpoint is keyed by its device key: a member device dials
        // it as the workspace's host, which is what its links admit.
        let server = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(iroh::SecretKey::from_bytes(&host_key.to_bytes()))
            .relay_mode(iroh::RelayMode::Disabled)
            .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .bind()
            .await
            .unwrap();
        let workspace = WorkspaceId::parse(governing.workspace()).unwrap();
        let topic = workspace_topic(workspace).unwrap();
        let hint = governing.hint(server.addr());
        let relay_root = dir.join("relay");
        let baseline = governing
            .peer()
            .admitted_workspace()
            .unwrap()
            .clone()
            .save();
        tilo_relay::store::WorkspaceStore::open(&relay_root, topic, true)
            .unwrap()
            .save_snapshot(&baseline)
            .unwrap();
        let authority: Authority = Arc::new(std::sync::Mutex::new(governing));
        let hosted = HostParts {
            authority: Arc::clone(&authority),
            host: host.clone(),
            host_key: host_key.clone(),
            serving: tilo_peer_state::direct::Serving::new(),
        };
        let join = JoinService(hosted.clone());
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
            vec![
                (
                    WORKSPACE_JOIN_ALPN.to_vec(),
                    Box::new(WorkspaceJoinProtocol::new(join)) as Box<dyn DynProtocolHandler>,
                ),
                (
                    tilo_peer_state::direct::DIRECT_ALPN.to_vec(),
                    Box::new(hosted.serving.clone()) as Box<dyn DynProtocolHandler>,
                ),
            ],
        )
        .await
        .unwrap();
        hosted.serve();
        Self {
            authority,
            relay,
            workspace,
            governor,
            governor_key,
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

    /// The governor's own device, linked to the workspace's host as another of
    /// its member devices: what it sends crosses the host to every member, and
    /// what they send reaches it. The endpoint the link dials from comes back
    /// with it: dropping an endpoint ends its links.
    async fn member_link(&self) -> (DirectSync, iroh::Endpoint) {
        let link = DirectSync::new(
            self.authority.lock().unwrap().peer().clone(),
            self.governor_key.clone(),
        );
        let (payload, _) =
            IrohWorkspaceHostHintV1::authenticate_transport_hint(&self.hint).unwrap();
        let endpoint = local_endpoint(&self.governor_key).await;
        let dialed = link.dial(&endpoint, payload.endpoint_addr).await.unwrap();
        let running = link.clone();
        tilo_rt::app().spawn(async move {
            let _ = dialed.run(&running).await;
        });
        (link, endpoint)
    }
}

fn assert_changed_custody_refused(joined: &Joined, workspace: WorkspaceId, actor: &str) {
    let mut custody = joined.custody.clone();
    // In memory only: this probe compares what it holds, never a disk.
    custody.custody = None;
    let workspace_cert = custody.device_cert.clone();
    let default_root = SigningKey::from_bytes(&[95; 32]);
    custody.device_cert.user_public_key = default_root.verifying_key().to_bytes();
    sign_device_cert(&mut custody.device_cert, &default_root).unwrap();
    let default_actor = tilo_action_broker::collab_app_broker::principal_id_for_root(
        custody.device_cert.user_public_key,
    );
    custody
        .workspace_device_certs
        .insert(workspace_topic(workspace).unwrap(), workspace_cert);
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
        own_host: std::sync::OnceLock::new(),
        this: std::sync::Weak::new(),
    };
    assert!(
        probe
            .action_workspace(actor, &workspace.to_string())
            .is_ok(),
        "the admitted selected root is independent of the default root"
    );
    assert!(
        probe
            .action_workspace(&default_actor, &workspace.to_string())
            .is_err(),
        "a default root must not borrow the selected root's admission"
    );
    let run = probe
        .admit_mcp_execution(&workspace.to_string(), actor)
        .unwrap();
    let retained = run.caller().clone();
    probe.recheck_mcp_executions(workspace).unwrap();
    assert!(retained.execution_is_live());
    // What this profile holds for the workspace is not what the run was admitted
    // with: the read succeeds and says the custody changed, so the run ends.
    let mut renewed = probe.custody.device_cert.clone();
    renewed.device_label = "a reissued certificate".into();
    sign_device_cert(&mut renewed, &default_root).unwrap();
    probe
        .custody
        .workspace_device_certs
        .insert(workspace_topic(workspace).unwrap(), renewed);
    assert!(
        probe.recheck_mcp_executions(workspace).is_err(),
        "a custody the read confirms is not what the run was admitted with"
    );
    assert!(
        !retained.execution_is_live(),
        "authority-side original-pin recheck closes retained custody without a wrapper call"
    );
    assert!(
        probe
            .action_workspace(actor, &workspace.to_string())
            .is_err(),
        "changed selected custody must not reuse the retained roster identity"
    );
    assert!(!probe.workspaces().iter().any(|held| held.id() == workspace));
}

fn assert_runtime_admission_revoked(joined: &Arc<Joined>, workspace: WorkspaceId, actor: &str) {
    use crate::standalone::RuntimeActionHost;
    use tilo_action_broker::{
        collab_app_broker::{AppBrokerState, PublishedAction, PublishedInput, PublishedSurface},
        collab_broker::CrdtRoute,
        test_support::{Workspace, WorkspaceSlot, WorkspaceTree},
    };
    use tilo_actions::remote::{
        ActionHost, ActionHostError, Caller, CatalogueQuery, InvokeRequest,
    };
    use tilo_actions::remote::{InvokeArgument, PendingPoll, RemoteActionTarget};

    let mut placed = Workspace::in_workspace(WorkspaceSlot(41), workspace, "Execution isolation");
    placed.set_owner(actor.to_owned());
    let workspace_id = workspace.to_string();
    let directory = tempfile::tempdir().unwrap();
    let mut tree = WorkspaceTree {
        persist_path: Some(directory.path().join("workspace.json")),
        ..WorkspaceTree::default()
    };
    tree.desktop.workspaces.push(placed);
    let membership = tree.shared();
    let origin = tilo_action_broker::policy_clock::OwnerPolicyClock::new()
        .unwrap()
        .new_origin();
    let admission = joined.admit_mcp_execution(&workspace_id, actor).unwrap();
    let mut services = membership.services();
    services.native_certificates = Some(Arc::new(super::admission_tests::FixtureAuthority {
        workspace: uuid::Uuid::parse_str(&workspace_id).unwrap(),
        cert: admission.certificate().clone(),
        time: origin.capture().unwrap(),
    }));
    let (broker, turn) = AppBrokerState::test_broker_with_authority_and_invoke_turn(
        membership.authority(),
        services,
    );
    let qualified = broker
        .certified_local_action_host(admission.caller(), admission.presentation())
        .unwrap();
    broker.replace_published_surfaces_for_test(vec![PublishedSurface {
        peer_id: 7,
        app_id: "so.tilo.files".into(),
        route: CrdtRoute::app("so.tilo.files", workspace_id.clone()),
        actions: vec![PublishedAction {
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            resource: "files.entry:/tmp/synthetic-report".into(),
            action: "refresh".into(),
            resource_kind: "files.entry".into(),
            resource_label: "Synthetic report".into(),
            action_label: "Refresh".into(),
            keywords: Vec::new(),
            icon: None,
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
    let owner = Caller::local_person(actor).in_workspace(workspace_id.clone());
    let session = Arc::new(crate::standalone::AdmittedRuntimeSession {
        policy_time: origin.capture().unwrap(),
        host: qualified,
        admission,
        agent: None,
        projection: Some(owner.clone()),
    });
    let host = RuntimeActionHost {
        broker,
        workspace: "private-profile-workspace".into(),
        joined: Arc::clone(joined),
        session: Some(Arc::clone(&session)),
    };
    let request = InvokeRequest {
        on_behalf_of: None,
        invocation: tilo_actions::InvocationId::new(),
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
    let id = request.invocation;
    let mut pending = host.invoke(&owner, request.clone());
    assert!(matches!(pending.poll_ready(), PendingPoll::Waiting));
    turn();
    // Answered once the admission is durable, from the journal's writer.
    let outcome = pending
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten();
    let Some(Ok(_receipt)) = outcome else {
        panic!("the synthetic app must accept the invocation: {outcome:?}");
    };
    assert!(invocations.try_recv().is_ok());
    assert!(host.observe(&owner, &id).is_some());
    joined
        .owner
        .invalidate_identity(WorkspaceId::parse(&workspace_id).unwrap());
    assert!(host.observe(&owner, &id).is_none());
    assert_eq!(
        host.cancel(&owner, &id)
            .wait_timeout(std::time::Duration::from_secs(30))
            .ok()
            .flatten()
            .expect("the host answers"),
        Err(ActionHostError::NotPermitted)
    );
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
async fn a_foreign_runtime_joins_a_hosted_workspace_as_an_editor_and_syncs_both_ways() {
    if crate::standalone::tests::isolated_signing_identity("standalone::joined::tests::a_foreign_runtime_joins_a_hosted_workspace_as_an_editor_and_syncs_both_ways").is_none() {
        return;
    }
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let governing = Box::pin(HostedWorkspace::start(directory.path())).await;
    let workspace = governing.workspace;

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
        issuer: None,
    };
    sign_device_cert(&mut device, &root_key).unwrap();
    let invitation = governing.invite(device.user_public_key);

    let custody = HostIdentity {
        device_cert: device.clone(),
        device_signing_key: device_key.clone(),
        workspace_device_certs: std::collections::HashMap::default(),
        workspace_revocations: std::collections::HashMap::default(),
        custody: None,
    };
    let own_workspace = WorkspaceId::parse("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb").unwrap();
    let joined = Joined::open(
        directory.path(),
        custody,
        local_endpoint(&device_key).await,
        BTreeSet::from([own_workspace]),
        (
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::new(FanoutState::new()),
        ),
    )
    .unwrap();
    let actor =
        tilo_action_broker::collab_app_broker::principal_id_for_root(device.user_public_key);
    assert!(
        joined
            .action_workspace(&actor, &workspace.to_string())
            .is_err(),
        "an invitation is not current admission"
    );
    assert_eq!(joined.join(&invitation).await.unwrap(), workspace);
    assert_eq!(
        joined
            .action_workspace(&actor, &workspace.to_string())
            .unwrap(),
        workspace.to_string()
    );
    assert!(
        joined
            .action_workspace("forged-profile-actor", &workspace.to_string())
            .is_err()
    );
    assert!(
        joined
            .action_workspace(&actor, &own_workspace.to_string())
            .is_err()
    );
    assert!(joined.action_workspace(&actor, "not-a-workspace").is_err());
    let execution = joined
        .admit_mcp_execution(&workspace.to_string(), &actor)
        .unwrap();
    let presentation = runtime_collab::CertifiedJoinIdentity::decode(execution.presentation())
        .unwrap()
        .unwrap();
    assert_eq!(presentation.workspace_id, workspace.to_string());
    let presented: DeviceCert = serde_json::from_slice(&presentation.device_certificate).unwrap();
    assert_eq!(presented, device);
    device_key
        .verifying_key()
        .verify_strict(
            &runtime_identity::collaboration_join_signing_bytes(&workspace.to_string()),
            &ed25519_dalek::Signature::from_slice(&presentation.proof).unwrap(),
        )
        .unwrap();
    let retained_caller = execution.caller().clone();
    {
        use tilo_action_broker::certificate_authority::CertificateAuthority as _;
        let origin = tilo_action_broker::policy_clock::OwnerPolicyClock::new()
            .unwrap()
            .new_origin();
        let authority = super::super::Certificates {
            workspace: own_workspace.to_string(),
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
                .authorize_join(
                    uuid::Uuid::parse_str(&workspace.to_string()).unwrap(),
                    &forged
                )
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
        joined.recheck_mcp_executions(workspace).unwrap();
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
            .admit_mcp_execution(&workspace.to_string(), "forged-profile-actor")
            .is_err()
    );
    joined
        .owner
        .with_current(workspace, |view| {
            let role = view
                .peer()
                .permissions(device.device_public_key, SystemTime::now())
                .map_err(|error| error.to_string())?
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
            .any(|held| held.id() == workspace),
        "the joined workspace is a workspace of this profile"
    );
    // The profile's record is an index and one file per workspace, not the one file a build
    // before the split wrote: read it as the profile does.
    let profile = tilo_peer_state::owner::open_file(&directory.path().join(JOINED_WORKSPACES))
        .expect("the profile's own record opens");
    assert!(
        profile.workspaces().contains(&workspace),
        "the enrollment is kept in the profile's own record"
    );

    // The governor's device is linked to the workspace's host as its other
    // members are; the workspace's history reaches the runtime over its own
    // link, which catches up from the host.
    let (governor, _governor_endpoint) = governing.member_link().await;
    let mut heard = governor.heard();
    let chat = tilo_test_env::within("the workspace's history", async {
        loop {
            if let Ok(chat) = joined.owner.with_current(workspace, |view| {
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
        .with_current(workspace, |view| {
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
            workspace: workspace.to_string(),
            operation: CommitOperation::Document {
                app: "so.tilo.chat".into(),
                record: String::new(),
                envelope,
            },
        })
        .await;
    receipt.expect("a member's write commits to the joined workspace");
    tilo_test_env::within("the write at the host's link", async {
        loop {
            if let Ok(heard) = heard.recv().await
                && heard.author == device.device_public_key
            {
                break;
            }
        }
    })
    .await;

    // A sealed write, as an SDK app seals it under a key only the workspace's
    // key holders have: this member's owner and the host keep only its
    // ciphertext, and the governor receives it sealed.
    let channel = collab_types::document_channel_id("so.tilo.chat", &workspace.to_string(), "");
    let content = channel_crypto::ChannelGroup::create(
        channel,
        channel_crypto::MemberKey::generate("governor").unwrap(),
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
        .with_current(workspace, |view| {
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
            workspace: workspace.to_string(),
            operation: CommitOperation::Document {
                app: "so.tilo.chat".into(),
                record: String::new(),
                envelope,
            },
        })
        .await
        .expect("a keyless member keeps a sealed write as ciphertext");
    tilo_test_env::within("the sealed write at the host's link", async {
        loop {
            if let Ok(heard) = heard.recv().await
                && heard.author == device.device_public_key
                && let NeighbourMsg::AppDocument { envelope, .. } = heard.message
                && envelope.encryption.is_some()
            {
                break;
            }
        }
    })
    .await;
    assert_changed_custody_refused(&joined, workspace, &actor);
    assert_runtime_admission_revoked(&joined, workspace, &actor);
    assert!(
        !retained_caller.execution_is_live(),
        "native enrollment fence synchronously closes captured run custody"
    );
    assert!(execution.check(&joined).is_err());
    assert!(
        joined
            .admit_mcp_execution(&workspace.to_string(), &actor)
            .is_err()
    );
    assert!(
        !joined
            .workspaces()
            .iter()
            .any(|held| held.id() == workspace),
        "broker snapshots must drop stale admission too"
    );
    assert!(
        joined
            .action_workspace(&actor, &workspace.to_string())
            .is_err(),
        "stale identity removes the selected audience"
    );
    drop(governor);
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

const SDK_TEST: &str =
    "standalone::joined::tests::a_live_sdk_app_in_a_joined_workspace_gets_its_keys_from_the_device";
const SDK_HELPER: &str = "TILO_JOINED_SDK_HELPER";
const SDK_APP: &str = "so.tilo.chat";
const SDK_WRITTEN: &str = "sealed by a live SDK app in a joined workspace";

#[test]
fn a_live_sdk_app_in_a_joined_workspace_gets_its_keys_from_the_device() {
    // Helper role: an ordinary SDK app in the joined workspace writes once. The
    // SDK holds the write until this device's channel service hands it the
    // route's keys, then seals and sends it.
    if std::env::var_os(SDK_HELPER).is_some() {
        let mut manager = ui::collaboration::CollaborationManager::new();
        super::super::tests::wait_until_joined(&mut manager);
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
    let (_, device) =
        tilo_identity_host::broker::trusted_current_device(session.workspace()).unwrap();
    let governor_root = profile.parent().unwrap().join("governor");
    std::fs::create_dir_all(&governor_root).unwrap();
    let governing = tilo_rt::app()
        .block_on(Box::pin(HostedWorkspace::start(&governor_root)))
        .unwrap();
    let workspace = session.joiner().unwrap()(&governing.invite(device.user_public_key)).unwrap();
    assert_eq!(workspace, governing.workspace.to_string());
    let (governor, _governor_endpoint) = tilo_rt::app().block_on(governing.member_link()).unwrap();
    let mut heard = governor.heard();
    // A member writes only into a record it holds, and joining brings the
    // workspace's governance alone: the chat record arrives through the workspace's
    // link to its host, which catches up from what the host keeps. The app opens
    // in the workspace as it stands then.
    tilo_rt::app()
        .block_on(tilo_test_env::within("the workspace's history", async {
            while session
                .joined
                .owner
                .with_current(governing.workspace, |view| {
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
        .env(
            tilo_test_env::hang::HANG_BOUND_ENV,
            super::super::tests::helper_hang_bound(),
        )
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log.try_clone().unwrap()))
        .stderr(std::process::Stdio::from(log));
    let mut app = crate::unix::launch(
        command,
        SDK_APP,
        &workspace,
        &crate::unix::ServiceSockets {
            collab: session.broker.app_socket(),
            identity: &session.identity_socket,
            blobs: &session.blob_socket,
        },
    )
    .unwrap();

    // This device's channel service hands the app its route's keys; the
    // app's write then arrives sealed, kept by the host as ciphertext.
    let sealed = tilo_rt::app()
        .block_on(tilo_test_env::within(
            "the SDK app's sealed write at the workspace",
            async {
                loop {
                    if let Ok(heard) = heard.recv().await
                        && heard.author == device.device_public_key
                        && let NeighbourMsg::AppDocument { envelope, .. } = heard.message
                        && envelope.encryption.is_some()
                    {
                        break *envelope;
                    }
                }
            },
        ))
        .unwrap();
    let _ = app.kill();
    let _ = app.wait();

    // The keys the app sealed with are this device's own: the joined workspace's
    // channel service reads what the app wrote.
    let keys = session
        .joined
        .channel_keys()
        .get()
        .expect("the joined device runs its channel service");
    let plaintext = tilo_rt::app()
        .block_on(tilo_test_env::within(
            "the device's keys over the SDK app's sealed write",
            async {
                loop {
                    if let Ok(plaintext) = keys.open_history_write(governing.workspace, &sealed) {
                        break plaintext;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            },
        ))
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
    drop(governor);
    tilo_rt::app()
        .block_on(async { governing.relay.shutdown().await })
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
fn the_workspaces_the_desktop_session_enrolled_are_refused() {
    let directory = tempfile::tempdir().unwrap();
    assert!(desktop_enrolled(directory.path()).unwrap().is_empty());
    let record = tilo_peer_state::owner::owner_file_of(
        &directory
            .path()
            .join(super::super::workspace::DESKTOP_WORKSPACE),
    );
    std::fs::write(&record, br#"{"revision": 1, "rooms": {}, "pending": {}}"#).unwrap();
    assert!(desktop_enrolled(directory.path()).unwrap().is_empty());
    std::fs::write(&record, b"not an owner record").unwrap();
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
async fn a_joined_workspace_s_record_content_crosses_the_host_both_ways() {
    use tilo_action_broker::record_content::PIECE_BYTES;
    use tilo_content::hash_bytes;
    use tilo_peer_state::peer_handle;

    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let governing = Box::pin(HostedWorkspace::start(directory.path())).await;
    let workspace = governing.workspace;
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
        issuer: None,
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
            custody: None,
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
    assert_eq!(joined.join(&invitation).await.unwrap(), workspace);
    let (governor, _governor_endpoint) = governing.member_link().await;
    let mut heard = governor.heard();
    let governor_handle = peer_handle(&governing.governor.device_public_key);
    let runtime_handle = peer_handle(&device.device_public_key);
    let route = tilo_action_broker::route::CrdtRoute::record(
        "so.tilo.slides",
        workspace.to_string(),
        "deck",
    );

    // The runtime asks the workspace for a picture; the governor's device sends it.
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
                    if let Ok(heard) = heard.recv().await
                        && heard.author == device.device_public_key
                        && let NeighbourMsg::RecordContentWant {
                            requester,
                            hash: asked,
                            ..
                        } = heard.message
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
        governor.send(piece).unwrap();
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
    governor.send(want).unwrap();
    let mut gathered = vec![0_u8; picture.len()];
    let mut received = 0;
    tilo_test_env::within("the picture at the governor", async {
        while received < picture.len() {
            let Ok(heard) = heard.recv().await else {
                panic!("the governor's link ended");
            };
            // The host, which keeps none of it, says so: the picture
            // comes from the runtime.
            let NeighbourMsg::RecordContentPiece {
                requester,
                offset,
                bytes,
                ..
            } = heard.message
            else {
                continue;
            };
            assert_eq!(heard.author, device.device_public_key);
            assert_eq!(requester, governor_handle);
            let start = usize::try_from(offset).unwrap();
            gathered[start..start + bytes.len()].copy_from_slice(&bytes);
            received += bytes.len();
        }
    })
    .await;
    assert_eq!(gathered, picture);
}

/// Two devices that reach each other only through the workspace's host: a want
/// whose holder is offline waits (the host keeps none of it), and the
/// moment the holder connects to the host the want is asked of it, and the
/// content arrives: waiting, then fetching, then here.
#[tokio::test(flavor = "multi_thread")]
async fn a_want_through_the_host_waits_for_its_offline_holder_and_arrives_when_it_connects() {
    use tilo_action_broker::record_content::PIECE_BYTES;
    use tilo_content::{ContentState, hash_bytes};
    use tilo_peer_state::peer_handle;

    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let governing = Box::pin(HostedWorkspace::start(directory.path())).await;
    let workspace = governing.workspace;
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
        issuer: None,
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
            custody: None,
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
    assert_eq!(joined.join(&invitation).await.unwrap(), workspace);
    // The holder is offline: it is no device of the workspace's link yet.
    let governor_handle = peer_handle(&governing.governor.device_public_key);
    let runtime_handle = peer_handle(&device.device_public_key);
    let route = tilo_action_broker::route::CrdtRoute::record(
        "so.tilo.ebook",
        workspace.to_string(),
        "library",
    );
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
    let (holder, _holder_endpoint) = governing.member_link().await;
    let mut heard = holder.heard();
    tilo_test_env::within("the want asked of the holder", async {
        loop {
            if let Ok(heard) = heard.recv().await
                && heard.author == device.device_public_key
                && let NeighbourMsg::RecordContentWant {
                    requester,
                    hash: asked,
                    ..
                } = heard.message
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
        holder.send(piece).unwrap();
    }
    let arrived = tilo_test_env::within("the book at the runtime", news.recv())
        .await
        .unwrap();
    assert_eq!(arrived.state, ContentState::Resolved, "here");
    assert_eq!(arrived.bytes, book);
}

/// A write as the SDK signs one, of `app`'s record in `workspace`, signed by `key`.
fn signed_write(
    workspace: WorkspaceId,
    app: &str,
    key: &SigningKey,
) -> tilo_peer_state::owner_commit::OwnerCommitRequest {
    const RECORD: &str = "tab:0000000000000001";
    let workspace = workspace.to_string();
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "text", "written").unwrap();
    doc.commit();
    let now = SystemTime::now();
    let sequence = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_micros();
    let mut envelope = collab_types::SignedOp::new_unsigned(
        collab_types::DocumentMutation::Snapshot(doc.save())
            .encode()
            .unwrap(),
        collab_types::document_artifact_id(app, &workspace, RECORD),
        collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
        collab_types::document_attestation_id(app, &workspace, RECORD),
        collab_types::OpKind::Change,
        now,
        u64::try_from(sequence).unwrap(),
    );
    collab_types::sign_op(&mut envelope, key).unwrap();
    tilo_peer_state::owner_commit::OwnerCommitRequest {
        workspace,
        operation: CommitOperation::Document {
            app: app.into(),
            record: RECORD.into(),
            envelope: Box::new(envelope),
        },
    }
}

/// The profile's workspace is placed on the workspace host the profile runs and held
/// as a joined workspace, admitted like any other; placing it again finds it.
#[tokio::test(flavor = "multi_thread")]
async fn the_profile_workspace_is_placed_once_on_its_own_host_and_held() {
    let workspace = WorkspaceId::new();
    let (joined, _directory) = Joined::test_hosting(workspace).await;
    assert!(joined.hosted_here(workspace));
    assert!(joined.workspace(workspace).is_some());
    joined.place(workspace, "Home").await.unwrap();
    assert_eq!(joined.owner.workspaces(), vec![workspace]);
    let actor = tilo_action_broker::collab_app_broker::principal_id_for_root(
        joined.test_certificate().user_public_key,
    );
    assert_eq!(
        joined
            .action_workspace(&actor, &workspace.to_string())
            .unwrap(),
        workspace.to_string()
    );
    assert!(joined.requires_workspace(&workspace.to_string()));
    assert!(
        !joined.records_effects(&workspace.to_string()),
        "the profile keeps its own workspace's effect records"
    );
}

/// An app's write to a record the profile's workspace does not hold yet is
/// taken only in a kind its installed manifest declares, and only signed by
/// this device; nothing refused reaches the host.
#[tokio::test(flavor = "multi_thread")]
async fn a_write_to_a_record_the_workspace_lacks_is_taken_only_in_a_declared_kind() {
    let workspace = WorkspaceId::new();
    let (joined, _directory) = Joined::test_hosting(workspace).await;
    let key = joined.custody.device_signing_key.clone();
    for (app, kind) in [
        ("dev.tilo.declared", None),
        ("dev.tilo.declared", Some("dev.tilo.other")),
        ("dev.tilo.not-installed", Some("dev.tilo.note")),
    ] {
        let refused = joined
            .commit_with_record_kind(signed_write(workspace, app, &key), kind.map(str::to_owned))
            .await
            .unwrap_err();
        assert!(refused.contains("kind"), "{app} {kind:?}: {refused}");
    }
    let stranger = SigningKey::from_bytes(&[7; 32]);
    let refused = joined
        .commit_with_record_kind(
            signed_write(workspace, "dev.tilo.declared", &stranger),
            Some("dev.tilo.note".into()),
        )
        .await
        .unwrap_err();
    assert!(refused.contains("signer"), "{refused}");
}

/// The workspaces the desktop session governs here are refused, writes included.
#[tokio::test(flavor = "multi_thread")]
async fn writes_to_a_workspace_the_desktop_session_governs_are_refused() {
    let workspace = WorkspaceId::new();
    let (empty, directory) = Joined::test_empty().await;
    let joined = Joined::open(
        directory.path(),
        empty.custody.clone(),
        empty.endpoint(),
        BTreeSet::from([workspace]),
        (
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::new(FanoutState::new()),
        ),
    )
    .unwrap();
    assert!(joined.requires_workspace(&workspace.to_string()));
    let refused = joined
        .commit(signed_write(
            workspace,
            "dev.tilo.declared",
            &empty.custody.device_signing_key,
        ))
        .await
        .unwrap_err();
    assert!(refused.contains("read only"), "{refused}");
}
