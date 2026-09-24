//! Literal private actions through the production certified relay/broker/owner path.
use super::*;
use crate::{
    action_host::{ActionHost, Authority},
    collaboration::{CollaborationAdminCommand, CollaborationAdminHandle, CollaborationService},
    committed_authority::CommittedAuthority,
    host_signing::{HostSigning, SignatureMailbox},
    owner_commit::OwnerCommitMailbox,
};
use ed25519_dalek::SigningKey;
use qos_action_broker::collaboration_admin::APP_ID;
use qos_peer_state::room::media;
use tilo_actions::{InvocationId, InvocationUpdate};
use tilo_relay::{
    Envelope,
    peer_actions::{ActionMessage, DirectedAction},
    transport_v2::EnvelopeV2,
};
use tilo_user_identity::{DeviceCert, sign_device_cert};
use ui::automerge::{AutoCommit, ObjType, ROOT, ReadDoc, transaction::Transactable};
use uuid::Uuid;

struct Harness {
    owner: SessionOwner,
    host: LocalHost,
    committed: CommittedAuthority,
    runtime: ActionHost,
    admin: tokio::sync::mpsc::Receiver<CollaborationAdminCommand>,
    commits: OwnerCommitMailbox,
    signatures: SignatureMailbox,
    network: tokio::sync::mpsc::Receiver<qos_peer_state::NeighbourMsg>,
    service: CollaborationService,
    dispatched: usize,
}

fn add_channel(owner: &mut SessionOwner, channel: &str) {
    let mut peer = owner.peer().clone();
    let mut chat = AutoCommit::load(&peer.document("so.tilo.chat", "").unwrap()).unwrap();
    let channels = chat.get(ROOT, "channels").unwrap().unwrap().1;
    let object = chat.put_object(&channels, channel, ObjType::Map).unwrap();
    chat.put(&object, "kind", "voice").unwrap();
    chat.put(&object, "workspace", "home").unwrap();
    peer.publish_document(
        "so.tilo.chat",
        "",
        chat.save(),
        &owner.identity.native,
        web_time::SystemTime::now(),
    )
    .unwrap();
    owner
        .commit_mutation_candidate(peer, owner.directory().to_vec())
        .unwrap();
}

fn sibling(owner: &mut SessionOwner, seed: u8) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let mut cert = owner.peer().authority().clone();
    cert.device_public_key = key.verifying_key().to_bytes();
    cert.device_label = format!("actual broker transport {seed}");
    sign_device_cert(&mut cert, &owner.identity.root).unwrap();
    let mut peer = owner.peer().clone();
    peer.admit_certificate(
        cert.clone(),
        &owner.identity.native,
        web_time::SystemTime::now(),
    )
    .unwrap();
    owner
        .commit_mutation_candidate(peer, owner.directory().to_vec())
        .unwrap();
    (key, cert)
}

async fn client(
    host: &LocalHost,
    owner: &SessionOwner,
    key: SigningKey,
    cert: DeviceCert,
) -> RelayClient {
    let endpoint = Endpoint::builder(presets::Minimal)
        .relay_mode(host.mode.clone())
        .bind()
        .await
        .unwrap();
    let client = RelayClient::connect_v2_to_enrolled_home_peer(
        endpoint,
        host.relay.node_addr(),
        owner.peer().authority().user_public_key,
        owner.host_certificate().device_public_key,
        cert,
        key,
        RevocationList::new(),
    )
    .await
    .unwrap();
    let room = Uuid::parse_str(owner.peer().room()).unwrap();
    client
        .send(Envelope::Subscribe {
            workspace_id: owner.workspace_id(),
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !matches!(client.recv().await.unwrap(), Envelope::Ready { .. }) {}
    })
    .await
    .unwrap();
    client
        .send_action(EnvelopeV2::EnableActions { room })
        .unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), client.recv_action())
            .await
            .unwrap(),
        Some(EnvelopeV2::ActionsReady { .. })
    ));
    client
        .send_action(EnvelopeV2::EnableBrokerClient { room })
        .unwrap();
    client
}

impl Harness {
    async fn response(
        &mut self,
        caller: &RelayClient,
        request: Uuid,
        terminal: bool,
    ) -> ActionMessage {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                tokio::select! {
                    reply = caller.recv_action() => match reply.unwrap() {
                        EnvelopeV2::Delivered { action, .. } if action.request_id == request => {
                            if !terminal || matches!(&action.message, ActionMessage::Rejected { .. } | ActionMessage::Observation { update: Some(InvocationUpdate::Succeeded { .. } | InvocationUpdate::Failed { .. } | InvocationUpdate::Cancelled { .. }), .. }) {
                                return action.message;
                            }
                        }
                        EnvelopeV2::Unavailable { request_id, reason, .. } if request_id == request => panic!("actual relay rejected request: {reason:?}"),
                        _ => {}
                    },
                    command = self.admin.recv() => {
                        let command = command.unwrap();
                        self.dispatched += 1;
                        let response = handle_collaboration_admin(&mut self.owner, &self.committed, &self.host, &self.service, command.request).await;
                        self.runtime.committed(&self.owner).unwrap();
                        let _ = command.reply.send(response);
                    }
                    pending = self.commits.recv() => {
                        let update = self.committed.begin(&self.owner).unwrap();
                        let complete = pending.unwrap().apply_deferred(&mut self.owner);
                        update.publish(&self.owner).unwrap();
                        complete().unwrap();
                    }
                    pending = self.signatures.recv() => {
                        let update = self.committed.begin(&self.owner).unwrap();
                        let (complete, result) = pending.unwrap().apply(&mut self.owner);
                        update.publish(&self.owner).unwrap();
                        complete();
                        result.unwrap();
                    }
                    _ = self.network.recv() => {}
                }
            }
        }).await.expect("real broker must finish through owner mailboxes")
    }
}

fn send(
    caller: &RelayClient,
    room: Uuid,
    source: [u8; 32],
    target: [u8; 32],
    message: ActionMessage,
) -> Uuid {
    let request_id = Uuid::new_v4();
    caller
        .send_action(EnvelopeV2::Directed(DirectedAction {
            room,
            request_id,
            source,
            target,
            message,
        }))
        .unwrap();
    request_id
}

fn invoke(
    host: Uuid,
    peer: u64,
    room: Uuid,
    invocation: InvocationId,
    action: &str,
    argument: serde_json::Value,
) -> ActionMessage {
    ActionMessage::InvokeBound {
        host,
        invocation,
        app_peer: peer.to_string(),
        app_id: APP_ID.into(),
        record_id: "".into(),
        resource: format!("workspace:{room}"),
        action: action.into(),
        argument: Some(argument),
        confirmation: None,
        revision: None,
    }
}
fn success(message: ActionMessage) -> media::MediaCallResultV1 {
    let ActionMessage::Observation {
        update: Some(InvocationUpdate::Succeeded { output, .. }),
        ..
    } = message
    else {
        panic!("expected real broker success, got {message:?}")
    };
    serde_json::from_value(output.value.unwrap()).unwrap()
}
fn counters(result: &media::MediaCallResultV1) -> serde_json::Value {
    json!({"call_id": hex::encode(result.call_id), "expected_authorization_revision": result.authorization_revision, "expected_epoch": result.epoch})
}

#[tokio::test(flavor = "current_thread")]
async fn room_media_broker_certified_literals_persist_retry_and_reject() {
    tokio::task::LocalSet::new().run_until(async {
        let directory = tempfile::tempdir().unwrap();
        let owner_root = directory.path().join("owner");
        let mut owner = SessionOwner::initialize(&owner_root).unwrap();
        owner.activate_delivery().unwrap();
        add_channel(&mut owner, "voice");
        add_channel(&mut owner, "additional");
        let (recipient_key, recipient_cert) = sibling(&mut owner, 211);
        let (other_key, other_cert) = sibling(&mut owner, 212);
        let source = owner.peer().authority().device_public_key;
        let room = Uuid::parse_str(owner.peer().room()).unwrap();
        let target = recipient_cert.device_public_key;
        let host = LocalHost::start(&owner).await.unwrap();
        let caller = client(&host, &owner, owner.identity.native.clone(), owner.peer().authority().clone()).await;
        let other = client(&host, &owner, other_key, other_cert.clone()).await;
        let recipient = client(&host, &owner, recipient_key, recipient_cert).await;
        let session = Arc::new(RelayCoeditSession::join(recipient, owner.workspace_id(), target).unwrap());
        assert!(matches!(session.recv().await, Some(Inbound::Ready { .. })));
        let committed = CommittedAuthority::new(&owner);
        let authority = Authority::new(committed.clone());
        authority.set_ready(true);
        let (commit, commits) = OwnerCommitMailbox::bounded(32);
        let (signing, signatures) = HostSigning::new(commit);
        let (network_sender, network) = tokio::sync::mpsc::channel(64);
        let (admin_handle, admin) = CollaborationAdminHandle::bounded(owner.peer().room().into(), 32, owner.supports_media_governance());
        let runtime = ActionHost::start_with_collaboration(&owner, authority, signing, network_sender, Some(admin_handle)).unwrap();
        let broker = runtime.broker.app_action_broker().unwrap();
        let (queue, durable_host) = broker.peer_action_attachment().unwrap();
        let (ready, mut readiness) = tokio::sync::watch::channel(false);
        let server_session = session.clone();
        let server = tokio::spawn(async move { queue.serve_with_readiness(server_session, room, durable_host, ready).await });
        tokio::time::timeout(Duration::from_secs(5), async { while !*readiness.borrow_and_update() { readiness.changed().await.unwrap(); } }).await.unwrap();
        let mut harness = Harness { owner, host, committed, runtime, admin, commits, signatures, network, service: CollaborationService::new(vec![]), dispatched: 0 };
        let request = send(&caller, room, source, target, ActionMessage::CatalogueRefresh { scope: None });
        let ActionMessage::BrokerCatalogue { host: advertised_host, catalogue } = harness.response(&caller, request, false).await else { panic!("native durable catalogue absent") };
        assert_eq!(advertised_host, durable_host);
        let surface = catalogue["surfaces"].as_array().unwrap().iter().find(|s| s["app_id"] == APP_ID).unwrap();
        let live_surface = broker.surfaces().into_iter().find(|s| s.app_id == APP_ID).unwrap();
        let peer = surface["peer_id"].as_u64().unwrap();
        assert_eq!(peer, live_surface.peer_id);
        assert_eq!(surface["actions"].as_array().unwrap().len(), 10);
        for literal in ["media.call.create", "media.call.end", "media.call.restart", "media.call.directions.set", "media.call.participant.denial.set"] {
            assert!(surface["actions"].as_array().unwrap().iter().any(|action| action["action"] == literal), "canonical native catalogue missing {literal}: {surface}");
        }
        let create = invoke(durable_host, peer, room, InvocationId::new(), "media.call.create", json!({"app_id":"so.tilo.chat", "record_id":"", "channel_id":"voice", "allowed":{"publish":true,"subscribe":true}}));
        let request = send(&caller, room, source, target, create.clone());
        let created = success(harness.response(&caller, request, true).await);
        assert_eq!(created.epoch, 1);
        assert_eq!(harness.dispatched, 1);
        let generation = harness.owner.committed_generation();
        let request = send(&caller, room, source, target, create);
        assert_eq!(success(harness.response(&caller, request, true).await), created);
        assert_eq!(harness.owner.committed_generation(), generation);
        assert_eq!(harness.dispatched, 1, "durable journal retry must not redispatch");

        let mut current = created.clone();
        for literal in ["media.call.end", "media.call.restart", "media.call.directions.set", "media.call.participant.denial.set"] {
            let mut argument = counters(&current);
            if literal == "media.call.directions.set" { argument["allowed"] = json!({"publish":false,"subscribe":true}); }
            if literal == "media.call.participant.denial.set" { argument["person_root"] = json!(hex::encode(other_cert.user_public_key)); argument["denied"] = json!({"publish":true,"subscribe":false}); }
            let request = send(&caller, room, source, target, invoke(durable_host, peer, room, InvocationId::new(), literal, argument));
            let next = success(harness.response(&caller, request, true).await);
            assert_eq!(next.call_id, created.call_id);
            assert_eq!(next.epoch, current.epoch + 1);
            current = next;
        }
        assert_eq!(harness.dispatched, 5);
        let policy = harness.owner.peer().validated_media_policy().unwrap().unwrap();
        assert_eq!(policy.calls[&created.call_id].epoch, current.epoch);
        assert_eq!(harness.owner.collaboration_state().administration.len(), 5);
        assert_eq!(harness.owner.collaboration_state().administration[0].media_result.as_ref(), Some(&created));
        assert_eq!(harness.owner.collaboration_state().administration[4].media_result.as_ref(), Some(&current));
        assert!(harness.owner.collaboration_state().administration.iter().all(|receipt| receipt.media_result.is_some()));

        let mut wrong_route = invoke(durable_host, peer, room, InvocationId::new(), "media.call.end", counters(&current));
        if let ActionMessage::InvokeBound { record_id, .. } = &mut wrong_route { *record_id = "wrong-record".into(); }
        let request = send(&caller, room, source, target, wrong_route);
        assert!(matches!(harness.response(&caller, request, true).await, ActionMessage::Rejected { .. }));
        assert_eq!(harness.dispatched, 5);
        let request = send(&other, room, other_cert.device_public_key, target, invoke(durable_host, peer, room, InvocationId::new(), "media.call.end", counters(&current)));
        assert!(matches!(harness.response(&other, request, true).await, ActionMessage::Observation { update: Some(InvocationUpdate::Failed { error, .. }), .. } if error.code == "workspace_collaboration_denied"));
        assert_eq!(harness.owner.peer().validated_media_policy().unwrap().unwrap(), policy);
        assert_eq!(harness.owner.collaboration_state().administration.len(), 5);

        harness.committed.with_owner(&mut harness.owner, |owner| {
            crate::room_media_policy::tests::edit_policy(owner, |policy| {
                for n in 1..64u8 {
                    let mut call = policy.calls[&created.call_id].clone();
                    call.phase = media::CallPhaseV1::Ended;
                    call.binding.channel_id = format!("ended-{n}");
                    call.binding.channel_object = format!("{}@ab", n + 1);
                    policy.calls.insert([n;32], call);
                }
            });
            Ok::<_, String>(())
        }).unwrap().unwrap();
        let request = send(&caller, room, source, target, invoke(durable_host, peer, room, InvocationId::new(), "media.call.create", json!({"app_id":"so.tilo.chat","record_id":"","channel_id":"additional","expected_authorization_revision":current.authorization_revision,"allowed":{"publish":true,"subscribe":true}})));
        assert!(matches!(harness.response(&caller, request, true).await, ActionMessage::Observation { update: Some(InvocationUpdate::Failed { error, .. }), .. } if error.code == "media_call_capacity"));
        assert_eq!(harness.owner.collaboration_state().administration.len(), 5);
        let stored_results = serde_json::to_value(&harness.owner.collaboration_state().administration).unwrap();
        harness.runtime.close(&harness.owner).unwrap();
        caller.close().await;
        other.close().await;
        session.close().await;
        server.abort();
        let _ = server.await;
        drop(harness.runtime);
        harness.host.shutdown().await.unwrap();
        drop(harness.owner);
        let reopened = SessionOwner::open(&owner_root).unwrap();
        assert_eq!(serde_json::to_value(&reopened.collaboration_state().administration).unwrap(), stored_results);
        assert_eq!(reopened.peer().validated_media_policy().unwrap().unwrap().calls[&created.call_id].epoch, current.epoch);
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn room_media_broker_legacy_native_catalogue_keeps_media_unadvertised() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let directory = tempfile::tempdir().unwrap();
            let owner = SessionOwner::initialize(&directory.path().join("owner")).unwrap();
            assert!(!owner.supports_media_governance());
            let authority = Authority::new(CommittedAuthority::new(&owner));
            authority.set_ready(true);
            let (commit, _commits) = OwnerCommitMailbox::bounded(32);
            let (signing, _signatures) = HostSigning::new(commit);
            let (network, _messages) = tokio::sync::mpsc::channel(64);
            let (admin, _commands) = CollaborationAdminHandle::bounded(
                owner.peer().room().into(),
                32,
                owner.supports_media_governance(),
            );
            let mut runtime = ActionHost::start_with_collaboration(
                &owner,
                authority,
                signing,
                network,
                Some(admin),
            )
            .unwrap();
            let surface = runtime
                .broker
                .app_action_broker()
                .unwrap()
                .surfaces()
                .into_iter()
                .find(|s| s.app_id == APP_ID)
                .unwrap();
            assert_eq!(surface.actions.len(), 5);
            assert!(
                surface
                    .actions
                    .iter()
                    .all(|action| !action.action.starts_with("media."))
            );
            runtime.close(&owner).unwrap();
        })
        .await;
}
