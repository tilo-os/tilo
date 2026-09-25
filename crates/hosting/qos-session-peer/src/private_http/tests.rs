use super::*;
use crate::SessionOwner;
use crate::{
    action_host::{ActionHost, Authority},
    committed_authority::CommittedAuthority,
    host_signing::HostSigning,
    owner_commit::OwnerCommitMailbox,
};
#[tokio::test(flavor = "current_thread")]
async fn private_http_installs_only_actual_live_broker_and_close_is_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
    let authority = Authority::new(CommittedAuthority::new(&owner));
    authority.set_ready(true);
    let (mailbox, _commits) = OwnerCommitMailbox::bounded(8);
    let (signing, _signatures) = HostSigning::new(mailbox);
    let (sender, _network) = tokio::sync::mpsc::channel(32);
    let mut actions = ActionHost::start(&owner, authority, signing, sender).unwrap();
    let mut service = PrivateService::install(&actions).unwrap();
    assert!(!service.is_closed());
    service.close();
    assert!(service.is_closed());
    actions.close(&owner).unwrap();
}

use tilo_relay::delivery_v3::{
    auth, http_proof,
    private_wire::{self, PrivateOperation, PrivateScope},
};
use tilo_relay::peer_actions::ActionMessage;
use uuid::Uuid;
fn with_admitted<R>(
    owner: &SessionOwner,
    current: &CommittedAuthority,
    action: &DirectedAction,
    op: PrivateOperation,
    run: impl FnOnce(VerifiedRequest<'_>, &qos_peer_state::StatePeer) -> R,
) -> R {
    with_exact_admitted(owner, current, action, op, false, run)
}
fn with_exact_admitted<R>(
    owner: &SessionOwner,
    current: &CommittedAuthority,
    action: &DirectedAction,
    op: PrivateOperation,
    events: bool,
    run: impl FnOnce(VerifiedRequest<'_>, &qos_peer_state::StatePeer) -> R,
) -> R {
    with_signed_session(owner, current, |admission, context, peer, id, digest| {
        let bytes = private_wire::encode_request(
            op,
            PrivateScope {
                room: action.room,
                source: action.source,
                target: action.target,
            },
            action,
        )
        .unwrap();
        let request = if events {
            http_proof::Request::new("GET", private_wire::PRIVATE_EVENTS_TARGET, &[]).unwrap()
        } else {
            http_proof::Request::new("POST", op.path(), &bytes).unwrap()
        };
        let proof = http_proof::RequestProof::sign(
            id,
            digest,
            &request,
            context.now_ns,
            context.now_ns + 1_000_000_000,
            &owner.identity.native,
        )
        .unwrap();
        let verified = admission.admit(&proof, &request, context).unwrap();
        run(verified, peer)
    })
}
fn with_signed_session<R>(
    owner: &SessionOwner,
    current: &CommittedAuthority,
    run: impl FnOnce(
        &mut http_proof::HttpSessionAdmission,
        &auth::VerificationContext<'_>,
        &qos_peer_state::StatePeer,
        [u8; 16],
        [u8; 32],
    ) -> R,
) -> R {
    // Real signed session + actual current owner policy. This unit does not issue
    // an installed-data descriptor or a native memory qualification witness.
    let guard = current.read().unwrap();
    let now = tilo_relay::clock::now_ns();
    let authority = crate::owner::delivery::NativeDeliveryAuthority {
        view: guard.view(),
        workspace: owner.workspace_id(),
        host: owner.host_certificate(),
    };
    let binding = auth::Binding {
        workspace: owner.workspace_id(),
        workspace_root: guard.peer().authority().user_public_key,
        device: owner.identity.native_cert.device_public_key,
        device_root: owner.identity.native_cert.user_public_key,
        host: owner.host_certificate().device_public_key,
        host_root: owner.host_certificate().user_public_key,
        origin: "https://localhost:9443".into(),
        descriptor_generation: 1,
        descriptor_digest: [8; 32],
        server_boot_id: [9; 16],
        carrier: auth::Carrier::Https,
        requested_capabilities: 6,
        mandatory_capabilities: 6,
    };
    let revocations = tilo_user_identity::RevocationList::new();
    let credentials = auth::Credentials {
        device: &owner.identity.native_cert,
        host: owner.host_certificate(),
        device_revocations: &revocations,
        host_revocations: &revocations,
    };
    let context = auth::VerificationContext {
        binding: &binding,
        capabilities: auth::CapabilityPolicy {
            supported: 6,
            allowed: 6,
        },
        credentials,
        authority: &authority,
        now_ns: now,
    };
    let (challenge, server) =
        auth::Challenge::issue(&context, [2; 32], &owner.identity.host).unwrap();
    let client = auth::verify_server(
        &auth::ClientExpectations {
            scope: auth::ClientScope::from(&binding),
            capabilities: context.capabilities,
            credentials: context.credentials,
            authority: &authority,
            now_ns: now,
        },
        [2; 32],
        &server,
    )
    .unwrap()
    .client_proof(&owner.identity.native)
    .unwrap();
    let session = challenge.finish(&client, &context).unwrap();
    let id = session.session_id();
    let digest = http_proof::session_binding_digest(
        session.binding(),
        id,
        session.capabilities(),
        session.expires_at_ns(),
    )
    .unwrap();
    let mut admission = http_proof::HttpSessionAdmission::new(session, &context).unwrap();
    run(&mut admission, &context, guard.peer(), id, digest)
}

#[tokio::test(flavor = "current_thread")]
async fn private_http_real_broker_catalogue_and_unknown_status_preserve_correlation() {
    let dir = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
    owner.activate_delivery().unwrap();
    owner
        .install_delivery_transport("https://localhost:9443", 0, web_time::SystemTime::now())
        .unwrap();
    let current = CommittedAuthority::new(&owner);
    let authority = Authority::new(current.clone());
    authority.set_ready(true);
    let (mailbox, _commits) = OwnerCommitMailbox::bounded(8);
    let (signing, _signatures) = HostSigning::new(mailbox);
    let (sender, _network) = tokio::sync::mpsc::channel(32);
    let mut actions = ActionHost::start(&owner, authority, signing, sender).unwrap();
    // ActionHost registers its owner policy time, so the real broker refuses
    // legacy untimed work; submit as production (native.rs) does.
    let time = actions.registered_policy_time().clone();
    let mut service = PrivateService::install(&actions).unwrap();
    for op in [
        PrivateOperation::Catalogue,
        PrivateOperation::Status,
        PrivateOperation::Cancel,
    ] {
        let id = tilo_actions::InvocationId(Uuid::from_u128(333));
        let message = match op {
            PrivateOperation::Catalogue => ActionMessage::CatalogueRefresh { scope: None },
            PrivateOperation::Status => ActionMessage::Status { invocation: id },
            _ => ActionMessage::Cancel { invocation: id },
        };
        let action = DirectedAction {
            room: Uuid::parse_str(owner.peer().room()).unwrap(),
            request_id: Uuid::new_v4(),
            source: owner.identity.native_cert.device_public_key,
            target: owner.peer().authority().device_public_key,
            message,
        };
        assert_ne!(action.target, owner.host_certificate().device_public_key);
        let mut pending = with_admitted(&owner, &current, &action, op, |proof, peer| {
            service.submit_with_policy_time(
                proof,
                peer,
                Instant::now() + std::time::Duration::from_secs(5),
                time.clone(),
            )
        })
        .unwrap();
        let reply = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(reply) = pending.try_reply().unwrap() {
                    break reply;
                }
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(reply.request_id, action.request_id);
        assert_eq!(reply.source, action.target);
        assert_eq!(reply.target, action.source);
        service.observe_reply(&pending, &reply).unwrap();
        assert!(
            service.watches.is_empty(),
            "unknown, rejected and catalogue replies do not create watches"
        );
        service.pump().unwrap();
        assert!(
            with_exact_admitted(&owner, &current, &action, op, true, |proof, peer| service
                .poll_events(proof, peer))
            .unwrap()
            .is_none()
        );
        let encoded = pending.encode_reply(&reply).unwrap();
        assert_eq!(
            private_wire::decode_response(op, &action, &encoded).unwrap(),
            reply
        );
        match (&op, &reply.message) {
            (PrivateOperation::Catalogue, ActionMessage::BrokerCatalogue { catalogue, .. }) => {
                assert_eq!(catalogue["version"], 1)
            }
            (
                PrivateOperation::Status,
                ActionMessage::Observation {
                    invocation,
                    update,
                    uncertain,
                    ..
                },
            ) => {
                assert_eq!(*invocation, id);
                assert!(update.is_none());
                assert!(!*uncertain);
            }
            (
                PrivateOperation::Cancel,
                ActionMessage::Observation {
                    invocation,
                    update,
                    uncertain,
                    ..
                },
            ) => {
                assert_eq!(*invocation, id);
                assert!(update.is_none());
                assert!(!*uncertain);
            }
            (_, ActionMessage::Rejected { .. }) if op == PrivateOperation::Cancel => {}
            other => panic!("unexpected actual broker response {other:?}"),
        }
    }
    let action = DirectedAction {
        room: Uuid::parse_str(owner.peer().room()).unwrap(),
        request_id: Uuid::new_v4(),
        source: owner.identity.native_cert.device_public_key,
        target: owner.peer().authority().device_public_key,
        message: ActionMessage::Status {
            invocation: tilo_actions::InvocationId(Uuid::from_u128(333)),
        },
    };
    // The same 32 queue permits cover work, buffered replies, and replies
    // already consumed but still retained by the outgoing carrier custody.
    let mut retained = Vec::new();
    for _ in 0..32 {
        retained.push(
            with_admitted(
                &owner,
                &current,
                &action,
                PrivateOperation::Status,
                |proof, peer| {
                    service.submit_with_policy_time(
                        proof,
                        peer,
                        Instant::now() + std::time::Duration::from_secs(5),
                        time.clone(),
                    )
                },
            )
            .unwrap(),
        );
    }
    assert!(matches!(
        with_admitted(
            &owner,
            &current,
            &action,
            PrivateOperation::Status,
            |proof, peer| service.submit_with_policy_time(
                proof,
                peer,
                Instant::now() + std::time::Duration::from_secs(5),
                time.clone(),
            )
        ),
        Err(PrivateError::Broker(CarrierError::Capacity))
    ));
    let first = loop {
        if let Some(reply) = retained[0].try_reply().unwrap() {
            break reply;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    };
    assert!(matches!(
        with_admitted(
            &owner,
            &current,
            &action,
            PrivateOperation::Status,
            |proof, peer| service.submit_with_policy_time(
                proof,
                peer,
                Instant::now() + std::time::Duration::from_secs(5),
                time.clone(),
            )
        ),
        Err(PrivateError::Broker(CarrierError::Capacity))
    ));
    let custody = retained.remove(0);
    let encoded = custody.encode_reply(&first).unwrap();
    let (ingress, ingress_receiver) = crate::https::ingress_channel();
    let initial_bytes = ingress_receiver.available_bytes();
    // Actual HTTP response backing with the genuinely admitted native broker
    // pending above. This helper's outer proof header is not re-admission.
    let (buffered, client) = crate::https::tests::data_test_request(
        &ingress,
        "POST",
        PrivateOperation::Status.path(),
        &[],
        &[1],
        Instant::now() + std::time::Duration::from_secs(5),
    );
    buffered
        .respond_with_credits(
            hyper::StatusCode::OK,
            encoded,
            crate::https::ResponseCredits {
                private_reply: Some(custody),
                ..Default::default()
            },
        )
        .unwrap();
    let (_, body) = client.receive_body().await.unwrap();
    let cloned = body.clone();
    let slice = body.slice(0..1);
    drop(body);
    drop(cloned);
    assert!(ingress_receiver.available_bytes() < initial_bytes);
    assert!(matches!(
        with_admitted(
            &owner,
            &current,
            &action,
            PrivateOperation::Status,
            |proof, peer| service.submit_with_policy_time(
                proof,
                peer,
                Instant::now() + std::time::Duration::from_secs(5),
                time.clone(),
            )
        ),
        Err(PrivateError::Broker(CarrierError::Capacity))
    ));
    drop(slice);
    assert_eq!(ingress_receiver.available_bytes(), initial_bytes);
    let mut pending = with_admitted(
        &owner,
        &current,
        &action,
        PrivateOperation::Status,
        |proof, peer| {
            service.submit_with_policy_time(
                proof,
                peer,
                Instant::now() + std::time::Duration::from_secs(5),
                time.clone(),
            )
        },
    )
    .unwrap();
    service.close();
    assert!(
        pending.try_reply().is_err(),
        "closed private attachment must fence original pending reply"
    );
    assert!(
        pending.encode_reply(&first).is_err(),
        "close also fences retained response encoding"
    );
    let _ = actions.close(&owner);
}

#[test]
fn private_event_slots_preserve_original_scope_transition_and_custody() {
    // Queue choreography test with real cryptographic admission. The controlled
    // queue replies are not a claim of durable broker/worker execution.
    let dir = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
    owner.activate_delivery().unwrap();
    owner
        .install_delivery_transport("https://localhost:9443", 0, web_time::SystemTime::now())
        .unwrap();
    let current = CommittedAuthority::new(&owner);
    let other_current = CommittedAuthority::new(&owner);
    let (queue, mut receiver) = PeerActionQueue::channel();
    let host = Uuid::new_v4();
    let mut service = PrivateService {
        transitions: queue.carrier_transitions(),
        queue: queue.clone(),
        host,
        closed: Arc::new(AtomicBool::new(false)),
        watches: Vec::new(),
        next_watch: 1,
    };
    let invocation = tilo_actions::InvocationId(Uuid::new_v4());
    let action = DirectedAction {
        room: Uuid::parse_str(owner.peer().room()).unwrap(),
        request_id: Uuid::new_v4(),
        source: owner.identity.native_cert.device_public_key,
        target: owner.peer().authority().device_public_key,
        message: ActionMessage::Status { invocation },
    };
    with_signed_session(&owner, &current, |admission, context, peer, id, digest| {
        let bytes = private_wire::encode_request(
            PrivateOperation::Status,
            PrivateScope {
                room: action.room,
                source: action.source,
                target: action.target,
            },
            &action,
        )
        .unwrap();
        let exact =
            http_proof::Request::new("POST", PrivateOperation::Status.path(), &bytes).unwrap();
        let proof = http_proof::RequestProof::sign(
            id,
            digest,
            &exact,
            context.now_ns,
            context.now_ns + 1_000_000_000,
            &owner.identity.native,
        )
        .unwrap();
        let mut pending = service
            .submit(
                admission.admit(&proof, &exact, context).unwrap(),
                peer,
                Instant::now() + std::time::Duration::from_secs(5),
            )
            .unwrap();
        let incoming = receiver.try_recv().unwrap();
        let evidence = incoming.evidence.unwrap();
        let accepted = tilo_actions::InvocationUpdate::Accepted { invocation };
        incoming
            .reply
            .send(Ok(ActionMessage::Observation {
                host,
                invocation,
                update: Some(accepted.clone()),
                uncertain: false,
                cancellation_requested: false,
            }))
            .unwrap();
        drop(incoming._permit);
        let initial = pending.try_reply().unwrap().unwrap();
        let WatchUpdate::Installed(first_watch) =
            service.observe_reply(&pending, &initial).unwrap()
        else {
            panic!("missing installed watch")
        };
        assert_eq!(service.watches.len(), 1);
        drop(pending);
        let progress = tilo_actions::InvocationUpdate::Progress {
            invocation,
            sequence: 1,
            phase: "running".into(),
            completed: None,
            total: None,
            message: Some("one".into()),
        };
        queue.publish(evidence.clone(), progress.clone());
        service.pump().unwrap();
        let incoming = receiver.try_recv().unwrap();
        assert_eq!(incoming.poll, Some(invocation));
        assert_eq!(incoming.notification, Some(progress.clone()));
        incoming
            .reply
            .send(Ok(ActionMessage::Observation {
                host,
                invocation,
                update: Some(progress),
                uncertain: false,
                cancellation_requested: false,
            }))
            .unwrap();
        drop(incoming._permit);
        service.pump().unwrap();
        // Reusing the same original UUID/invocation creates a fresh watch only
        // after actual replacement; it must never inherit the previous fence.
        let proof = http_proof::RequestProof::sign(
            id,
            digest,
            &exact,
            context.now_ns,
            context.now_ns + 1_000_000_000,
            &owner.identity.native,
        )
        .unwrap();
        let mut fresh = service
            .submit(
                admission.admit(&proof, &exact, context).unwrap(),
                peer,
                Instant::now() + std::time::Duration::from_secs(5),
            )
            .unwrap();
        let next = receiver.try_recv().unwrap();
        next.reply.send(Ok(initial.message.clone())).unwrap();
        drop(next._permit);
        let fresh_reply = fresh.try_reply().unwrap().unwrap();
        assert_eq!(
            service.observe_reply(&fresh, &fresh_reply).unwrap(),
            WatchUpdate::Unchanged,
            "a busy old watch must retain its original owner fence"
        );
        assert_eq!(
            service.live_watch_ids().collect::<Vec<_>>(),
            vec![first_watch]
        );
        // Another genuinely admitted session of the same device cannot take
        // this session's retained event; its original Status remains available.
        assert!(
            with_exact_admitted(
                &owner,
                &other_current,
                &action,
                PrivateOperation::Status,
                true,
                |other, peer| service.poll_events(other, peer)
            )
            .unwrap()
            .is_none()
        );
        let poll =
            http_proof::Request::new("GET", private_wire::PRIVATE_EVENTS_TARGET, &[]).unwrap();
        let proof = http_proof::RequestProof::sign(
            id,
            digest,
            &poll,
            context.now_ns,
            context.now_ns + 1_000_000_000,
            &owner.identity.native,
        )
        .unwrap();
        let (custody, reply) = service
            .poll_events(admission.admit(&proof, &poll, context).unwrap(), peer)
            .unwrap()
            .unwrap();
        assert_eq!(custody.watch_id(), Some(first_watch));
        let body = custody.encode_reply(&reply).unwrap();
        let reply =
            private_wire::decode_response(PrivateOperation::Status, &action, &body).unwrap();
        assert_eq!(reply.request_id, action.request_id);
        // An idle old watch is refreshed, never replaced: a fresh Status
        // cannot rebind its original clock origin or disclosure fence.
        assert_eq!(
            service.observe_reply(&fresh, &fresh_reply).unwrap(),
            WatchUpdate::Unchanged,
            "a later same-ID response cannot replace the original watch fence"
        );
        assert_eq!(
            service.live_watch_ids().collect::<Vec<_>>(),
            vec![first_watch]
        );
        drop(fresh);
        let mut held = Vec::new();
        for _ in 0..31 {
            held.push(
                queue
                    .submit_certified(
                        evidence.clone(),
                        Instant::now() + std::time::Duration::from_secs(5),
                    )
                    .unwrap(),
            );
        }
        assert!(matches!(
            queue.submit_certified(
                evidence.clone(),
                Instant::now() + std::time::Duration::from_secs(5)
            ),
            Err(CarrierError::Capacity)
        ));
        drop(custody);
        held.push(
            queue
                .submit_certified(
                    evidence.clone(),
                    Instant::now() + std::time::Duration::from_secs(5),
                )
                .unwrap(),
        );
        assert_eq!(service.watches.len(), 1);
        let proof = http_proof::RequestProof::sign(
            id,
            digest,
            &poll,
            context.now_ns,
            context.now_ns + 1_000_000_000,
            &owner.identity.native,
        )
        .unwrap();
        assert!(
            service
                .poll_events(admission.admit(&proof, &poll, context).unwrap(), peer)
                .unwrap()
                .is_none(),
            "one event must not be delivered twice"
        );
        service.retire_session(id);
        assert!(service.watches.is_empty());
        for _ in 0..33 {
            queue.publish(evidence.clone(), accepted.clone());
        }
        assert!(
            service.pump().is_err(),
            "actual bounded broadcast lag must close attachment"
        );
        assert!(service.is_closed());
    });
}

#[tokio::test]
async fn actual_agent_read_custody_survives_final_server_body_slice() {
    use qos_action_broker::agent_reads::{AgentReadCustody, AgentReadReply};
    // This is queue/backing custody evidence, not agent execution evidence.
    let dir = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
    owner.activate_delivery().unwrap();
    owner
        .install_delivery_transport("https://localhost:9443", 0, web_time::SystemTime::now())
        .unwrap();
    let current = CommittedAuthority::new(&owner);
    let (queue, mut receiver) = PeerActionQueue::channel();
    let room = Uuid::parse_str(owner.peer().room()).unwrap();
    let mut service = PrivateService::from_queue_unqualified(queue, room).unwrap();
    let action = DirectedAction {
        room,
        request_id: Uuid::new_v4(),
        source: owner.identity.native_cert.device_public_key,
        target: owner.peer().authority().device_public_key,
        message: ActionMessage::Status {
            invocation: tilo_actions::InvocationId(Uuid::new_v4()),
        },
    };
    let mut submit = || {
        with_admitted(
            &owner,
            &current,
            &action,
            PrivateOperation::Status,
            |proof, peer| {
                service.submit(
                    proof,
                    peer,
                    Instant::now() + std::time::Duration::from_secs(5),
                )
            },
        )
    };
    let mut retained = Vec::new();
    let mut read_reply = None;
    for index in 0..32 {
        let pending = submit().unwrap();
        let request = receiver.try_recv().unwrap();
        if index == 0 {
            read_reply = Some(AgentReadReply::new(
                vec![71_u8],
                Some(AgentReadCustody::from_request(&request)),
            ));
            drop(request);
            drop(pending);
        } else {
            retained.push((pending, request));
        }
    }
    assert!(matches!(
        submit(),
        Err(PrivateError::Broker(CarrierError::Capacity))
    ));
    let (value, custody) = read_reply.unwrap().into_parts();
    let (ingress, ingress_receiver) = crate::https::ingress_channel();
    let idle = ingress_receiver.available_bytes();
    let (buffered, client) = crate::https::tests::data_test_request(
        &ingress,
        "POST",
        PrivateOperation::Status.path(),
        &[],
        &[1],
        Instant::now() + std::time::Duration::from_secs(5),
    );
    buffered
        .respond_with_credits(
            hyper::StatusCode::OK,
            value,
            crate::https::ResponseCredits {
                agent_read: custody,
                ..Default::default()
            },
        )
        .unwrap();
    let (_, body) = client.receive_body().await.unwrap();
    let clone = body.clone();
    let slice = body.slice(..1);
    drop(body);
    drop(clone);
    assert!(ingress_receiver.available_bytes() < idle);
    assert!(matches!(
        submit(),
        Err(PrivateError::Broker(CarrierError::Capacity))
    ));
    drop(slice);
    assert_eq!(ingress_receiver.available_bytes(), idle);
    assert!(
        submit().is_ok(),
        "only the final body backing releases the original broker slot"
    );
}
