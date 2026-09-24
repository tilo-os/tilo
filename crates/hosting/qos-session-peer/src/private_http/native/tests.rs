use super::*;
use crate::{SessionOwner, committed_authority::CommittedAuthority};
use tilo_relay::delivery_v3::{
    auth, http_proof,
    private_wire::{self, PrivateScope},
};
use tilo_relay::peer_actions::ActionMessage;
use uuid::Uuid;

fn context<R>(
    owner: &SessionOwner,
    current: &CommittedAuthority,
    binding: &auth::Binding,
    run: impl FnOnce(&auth::VerificationContext<'_>, &CommittedView) -> R,
) -> R {
    let guard = current.read().unwrap();
    let authority = crate::owner::delivery::NativeDeliveryAuthority {
        view: guard.view(),
        workspace: owner.workspace_id(),
        host: owner.host_certificate(),
    };
    let revocations = tilo_user_identity::RevocationList::new();
    let context = auth::VerificationContext {
        binding,
        capabilities: auth::CapabilityPolicy {
            supported: 6,
            allowed: 6,
        },
        credentials: auth::Credentials {
            device: &owner.identity.native_cert,
            host: owner.host_certificate(),
            device_revocations: &revocations,
            host_revocations: &revocations,
        },
        authority: &authority,
        now_ns: tilo_relay::clock::now_ns(),
    };
    run(&context, guard.view())
}
fn session(
    owner: &SessionOwner,
    current: &CommittedAuthority,
) -> (
    http_proof::HttpSessionAdmission,
    auth::Binding,
    [u8; 16],
    [u8; 32],
) {
    let binding = auth::Binding {
        workspace: owner.workspace_id(),
        workspace_root: owner.peer().authority().user_public_key,
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
    let (admission, id, digest) = context(owner, current, &binding, |context, _| {
        let (challenge, server) =
            auth::Challenge::issue(context, [2; 32], &owner.identity.host).unwrap();
        let proof = auth::verify_server(
            &auth::ClientExpectations {
                scope: auth::ClientScope::from(&binding),
                capabilities: context.capabilities,
                credentials: context.credentials,
                authority: context.authority,
                now_ns: context.now_ns,
            },
            [2; 32],
            &server,
        )
        .unwrap()
        .client_proof(&owner.identity.native)
        .unwrap();
        let session = challenge.finish(&proof, context).unwrap();
        let id = session.session_id();
        let digest = http_proof::session_binding_digest(
            session.binding(),
            id,
            session.capabilities(),
            session.expires_at_ns(),
        )
        .unwrap();
        (
            http_proof::HttpSessionAdmission::new(session, context).unwrap(),
            id,
            digest,
        )
    });
    (admission, binding, id, digest)
}
fn admitted<R>(
    owner: &SessionOwner,
    current: &CommittedAuthority,
    session: &mut (
        http_proof::HttpSessionAdmission,
        auth::Binding,
        [u8; 16],
        [u8; 32],
    ),
    action: &DirectedAction,
    poll: bool,
    run: impl FnOnce(VerifiedRequest<'_>, &CommittedView) -> R,
) -> R {
    let op = if matches!(action.message, ActionMessage::CatalogueRefresh { .. }) {
        PrivateOperation::Catalogue
    } else {
        PrivateOperation::Status
    };
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
    let exact = if poll {
        http_proof::Request::new("GET", private_wire::PRIVATE_EVENTS_TARGET, &[]).unwrap()
    } else {
        http_proof::Request::new("POST", op.path(), &bytes).unwrap()
    };
    context(owner, current, &session.1, |context, view| {
        let proof = http_proof::RequestProof::sign(
            session.2,
            session.3,
            &exact,
            context.now_ns,
            context.now_ns + 1_000_000_000,
            &owner.identity.native,
        )
        .unwrap();
        run(session.0.admit(&proof, &exact, context).unwrap(), view)
    })
}
fn policy(
    owner: &mut SessionOwner,
    current: &CommittedAuthority,
    cap: collab_types::caps::CapOverride,
) {
    use ui::automerge::{ROOT, ReadDoc, ScalarValue, transaction::Transactable};
    current
        .with_owner(owner, |owner| {
            let mut candidate = owner.peer().clone();
            let mut doc = candidate.admitted_room().unwrap().clone();
            doc.set_actor(ui::automerge::ActorId::from(
                owner.identity.native.verifying_key().to_bytes().to_vec(),
            ));
            let caps = doc.get(ROOT, "capabilities").unwrap().unwrap().1;
            doc.put(
                &caps,
                "private-result-test",
                ScalarValue::Bytes(serde_json::to_vec(&cap).unwrap()),
            )
            .unwrap();
            doc.commit();
            candidate
                .receive_from(
                    qos_peer_state::TransportOrigin::RelayDelta(
                        owner.identity.native.verifying_key().to_bytes(),
                    ),
                    qos_peer_state::NeighbourMsg::SyncResp {
                        room: owner.peer().room().into(),
                        changes: doc
                            .get_changes(&[])
                            .into_iter()
                            .map(|c| c.raw_bytes().to_vec())
                            .collect(),
                    },
                    web_time::SystemTime::now(),
                )
                .unwrap();
            owner
                .commit_mutation_candidate(candidate, owner.directory().to_vec())
                .unwrap();
        })
        .unwrap();
}
fn setup() -> (tempfile::TempDir, SessionOwner, CommittedAuthority) {
    let dir = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
    owner.activate_delivery().unwrap();
    owner
        .install_delivery_transport("https://localhost:9443", 0, web_time::SystemTime::now())
        .unwrap();
    let current = CommittedAuthority::new(&owner);
    (dir, owner, current)
}
#[test]
fn native_private_submission_retains_owner_policy_time_through_queue_and_terminal_fence() {
    let (_dir, owner, current) = setup();
    let owner = &owner;
    let mut session = session(owner, &current);
    let (queue, mut receiver) = PeerActionQueue::channel();
    let mut service = NativePrivateService {
        carrier: PrivateService::from_queue_unqualified(queue, Uuid::new_v4()).unwrap(),
        clock: current.policy_clock().unwrap(),
        fences: HashMap::new(),
    };
    let action = action(owner, ActionMessage::CatalogueRefresh { scope: None });
    let pending = admitted(
        owner,
        &current,
        &mut session,
        &action,
        false,
        |proof, view| {
            service.submit(
                proof,
                view,
                Instant::now() + std::time::Duration::from_secs(5),
            )
        },
    )
    .unwrap();
    let incoming = receiver.try_recv().unwrap();
    let queued_time = incoming
        .policy_time()
        .expect("native HTTPS queued work needs actual owner time")
        .clone();
    let retained_time = pending
        .carrier
        .pending
        .policy_time()
        .expect("pending reply must retain original owner time");
    assert!(queued_time.same_origin(retained_time));
    assert!(queued_time.now_ns().is_ok());
    current.invalidate();
    assert!(
        queued_time.now_ns().is_err(),
        "terminal owner denial must fence already queued policy decisions"
    );
    assert!(retained_time.now_ns().is_err());
}
#[test]
fn native_private_rejects_another_owner_clock_before_queue_submission() {
    let (_dir, owner, current) = setup();
    let unrelated = CommittedAuthority::new(&owner);
    let mut session = session(&owner, &current);
    let (queue, mut receiver) = PeerActionQueue::channel();
    let mut service = NativePrivateService {
        carrier: PrivateService::from_queue_unqualified(queue, Uuid::new_v4()).unwrap(),
        clock: unrelated.policy_clock().unwrap(),
        fences: HashMap::new(),
    };
    let action = action(&owner, ActionMessage::CatalogueRefresh { scope: None });
    assert!(
        admitted(
            &owner,
            &current,
            &mut session,
            &action,
            false,
            |proof, view| {
                service.submit(
                    proof,
                    view,
                    Instant::now() + std::time::Duration::from_secs(5),
                )
            }
        )
        .is_err()
    );
    assert!(matches!(
        receiver.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
}
fn action(owner: &SessionOwner, message: ActionMessage) -> DirectedAction {
    DirectedAction {
        room: owner.peer().room().parse().unwrap(),
        request_id: Uuid::new_v4(),
        source: owner.identity.native_cert.device_public_key,
        target: owner.peer().authority().device_public_key,
        message,
    }
}
fn write_allowed(owner: &SessionOwner, current: &CommittedAuthority) -> bool {
    use qos_action_broker::{policy::WorkspacePolicy, services::WorkspaceAuthority};
    let authority = crate::action_host::Authority::new(current.clone());
    authority.set_ready(true);
    let tree = authority.snapshot().unwrap();
    let workspace = &tree.workspaces[0];
    workspace.allows(
        &workspace.actor_for_person(&hex::encode(owner.identity.native_cert.user_public_key)),
        &collab_types::caps::ResourceRef::Action {
            app: "so.tilo.word-processor".into(),
            action: "replace-text".into(),
        },
        collab_types::caps::Verb::Write,
    )
}
fn output(
    pending: NativePendingPrivate,
    view: &CommittedView,
    reply: &DirectedAction,
) -> Result<(), PrivateError> {
    let (ingress, _receiver) = crate::https::ingress_channel();
    let (request, _client) = crate::https::tests::data_test_request(
        &ingress,
        "POST",
        pending.carrier.operation.path(),
        &[],
        &[1],
        Instant::now() + std::time::Duration::from_secs(5),
    );
    pending.respond(view, reply, request, Default::default())
}
#[tokio::test(flavor = "current_thread")]
async fn native_private_retained_real_broker_reply_rechecks_owner_before_output() {
    use crate::{
        action_host::Authority, host_signing::HostSigning, owner_commit::OwnerCommitMailbox,
    };
    let (_dir, mut owner, current) = setup();
    let authority = Authority::new(current.clone());
    authority.set_ready(true);
    let (mailbox, _commits) = OwnerCommitMailbox::bounded(8);
    let (signing, _signatures) = HostSigning::new(mailbox);
    let (sender, _network) = tokio::sync::mpsc::channel(32);
    let mut actions = ActionHost::start(&owner, authority, signing, sender).unwrap();
    let mut service = NativePrivateService::install(&actions).unwrap();
    let mut session = session(&owner, &current);
    let action = action(&owner, ActionMessage::CatalogueRefresh { scope: None });
    let mut pending = admitted(
        &owner,
        &current,
        &mut session,
        &action,
        false,
        |proof, view| {
            service.submit(
                proof,
                view,
                Instant::now() + std::time::Duration::from_secs(5),
            )
        },
    )
    .unwrap();
    let reply = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(reply) = pending.try_reply().unwrap() {
                break reply;
            }
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        reply.message,
        ActionMessage::BrokerCatalogue { .. }
    ));
    assert!(write_allowed(&owner, &current));
    policy(
        &mut owner,
        &current,
        collab_types::caps::CapOverride::restrict_role_in_app(
            collab_types::Role::Owner,
            "so.tilo.word-processor",
            collab_types::caps::Verb::Read,
        ),
    );
    assert!(
        !write_allowed(&owner, &current),
        "actual current action permission revoked"
    );
    // The same cryptographically admitted HTTP session still revalidates: only
    // action policy changed, not membership/device/transport scope.
    context(&owner, &current, &session.1, |context, _| {
        session.0.revalidate(context).unwrap()
    });
    assert!(
        output(pending, current.read().unwrap().view(), &reply).is_err(),
        "retained real broker catalogue escaped after action restriction"
    );
    let mut fresh = admitted(
        &owner,
        &current,
        &mut session,
        &action,
        false,
        |proof, view| {
            service.submit(
                proof,
                view,
                Instant::now() + std::time::Duration::from_secs(5),
            )
        },
    )
    .unwrap();
    let reply = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(reply) = fresh.try_reply().unwrap() {
                break reply;
            }
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
    assert!(
        output(fresh, current.read().unwrap().view(), &reply).is_ok(),
        "fresh authorized output remains live"
    );
    service.close();
    actions.close(&owner).unwrap();
}

fn accepted(host: Uuid, invocation: tilo_actions::InvocationId) -> ActionMessage {
    ActionMessage::Observation {
        host,
        invocation,
        update: Some(tilo_actions::InvocationUpdate::Accepted { invocation }),
        uncertain: false,
        cancellation_requested: false,
    }
}
#[test]
fn native_private_idle_watch_keeps_original_custody_after_fresh_status() {
    // Actual admission/current policy with controlled bounded queue replies;
    // this fixture does not claim a worker executed the supplied observation.
    let (_dir, mut owner, current) = setup();
    let mut session = session(&owner, &current);
    let (queue, mut receiver) = PeerActionQueue::channel();
    let host = Uuid::new_v4();
    let mut service = NativePrivateService {
        carrier: PrivateService::from_queue_unqualified(queue.clone(), host).unwrap(),
        clock: current.policy_clock().unwrap(),
        fences: HashMap::new(),
    };
    let invocation = tilo_actions::InvocationId(Uuid::new_v4());
    let action = action(&owner, ActionMessage::Status { invocation });
    let mut original = None;
    let mut old_id = None;
    for fresh_status in [false, true] {
        if fresh_status {
            policy(
                &mut owner,
                &current,
                collab_types::caps::CapOverride::restrict_role_in_app(
                    collab_types::Role::Owner,
                    "so.tilo.word-processor",
                    collab_types::caps::Verb::Read,
                ),
            );
            assert!(!write_allowed(&owner, &current));
        }
        let mut pending = admitted(
            &owner,
            &current,
            &mut session,
            &action,
            false,
            |proof, view| {
                service.submit(
                    proof,
                    view,
                    Instant::now() + std::time::Duration::from_secs(5),
                )
            },
        )
        .unwrap();
        let incoming = receiver.try_recv().unwrap();
        if !fresh_status {
            original = Some((
                incoming.evidence.clone().unwrap(),
                incoming.policy_time().unwrap().clone(),
            ));
        }
        incoming.reply.send(Ok(accepted(host, invocation))).unwrap();
        drop(incoming._permit);
        let reply = pending.try_reply().unwrap().unwrap();
        service
            .observe_reply(&pending, &reply, current.read().unwrap().view())
            .unwrap();
        let id = service.carrier.live_watch_ids().next().unwrap();
        if fresh_status {
            assert_eq!(
                Some(id),
                old_id,
                "fresh Status replaced original idle watch"
            );
        } else {
            old_id = Some(id);
        }
    }
    let (evidence, original_time) = original.unwrap();
    let progress = tilo_actions::InvocationUpdate::Progress {
        invocation,
        sequence: 1,
        phase: "running".into(),
        completed: None,
        total: None,
        message: Some("old invocation output".into()),
    };
    queue.publish(evidence, progress.clone());
    service.pump().unwrap();
    let incoming = receiver.try_recv().unwrap();
    assert!(incoming.policy_time().unwrap().same_origin(&original_time));
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
    assert!(
        admitted(
            &owner,
            &current,
            &mut session,
            &action,
            true,
            |proof, view| { service.poll_events(proof, view) }
        )
        .is_err(),
        "fresh Status replaced the older invocation disclosure policy"
    );
}

#[test]
fn native_private_ready_event_old_fence_cannot_be_blessed_by_same_uuid_fresh_status() {
    // Controlled real queue notifications isolate the carrier's retention race;
    // these replies are not a claim of real durable worker execution.
    let (_dir, mut owner, current) = setup();
    let mut session = session(&owner, &current);
    let (queue, mut receiver) = PeerActionQueue::channel();
    let host = Uuid::new_v4();
    let mut service = NativePrivateService {
        carrier: PrivateService::from_queue_unqualified(queue.clone(), host).unwrap(),
        clock: current.policy_clock().unwrap(),
        fences: HashMap::new(),
    };
    let invocation = tilo_actions::InvocationId(Uuid::new_v4());
    let action = action(&owner, ActionMessage::Status { invocation });
    let mut initial = admitted(
        &owner,
        &current,
        &mut session,
        &action,
        false,
        |proof, view| {
            service.submit(
                proof,
                view,
                Instant::now() + std::time::Duration::from_secs(5),
            )
        },
    )
    .unwrap();
    let incoming = receiver.try_recv().unwrap();
    let evidence = incoming.evidence.unwrap();
    incoming.reply.send(Ok(accepted(host, invocation))).unwrap();
    drop(incoming._permit);
    let reply = initial.try_reply().unwrap().unwrap();
    service
        .observe_reply(&initial, &reply, current.read().unwrap().view())
        .unwrap();
    drop(initial);
    let old_id = service.carrier.live_watch_ids().next().unwrap();
    let progress = tilo_actions::InvocationUpdate::Progress {
        invocation,
        sequence: 1,
        phase: "running".into(),
        completed: None,
        total: None,
        message: Some("private retained data".into()),
    };
    queue.publish(evidence, progress.clone());
    service.pump().unwrap();
    let incoming = receiver.try_recv().unwrap();
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
    assert!(write_allowed(&owner, &current));
    policy(
        &mut owner,
        &current,
        collab_types::caps::CapOverride::restrict_role_in_app(
            collab_types::Role::Owner,
            "so.tilo.word-processor",
            collab_types::caps::Verb::Read,
        ),
    );
    assert!(
        !write_allowed(&owner, &current),
        "actual current action permission revoked"
    );
    let mut fresh = admitted(
        &owner,
        &current,
        &mut session,
        &action,
        false,
        |proof, view| {
            service.submit(
                proof,
                view,
                Instant::now() + std::time::Duration::from_secs(5),
            )
        },
    )
    .unwrap();
    let incoming = receiver.try_recv().unwrap();
    incoming.reply.send(Ok(accepted(host, invocation))).unwrap();
    drop(incoming._permit);
    let reply = fresh.try_reply().unwrap().unwrap();
    service
        .observe_reply(&fresh, &reply, current.read().unwrap().view())
        .unwrap();
    assert_eq!(
        service.carrier.live_watch_ids().collect::<Vec<_>>(),
        vec![old_id]
    );
    assert_eq!(service.fences.len(), 1);
    assert!(
        admitted(
            &owner,
            &current,
            &mut session,
            &action,
            true,
            |proof, view| service.poll_events(proof, view)
        )
        .is_err(),
        "fresh Status blessed older Ready event"
    );
    assert!(output(fresh, current.read().unwrap().view(), &reply).is_ok());
    service.retire_session(session.2);
    assert!(service.fences.is_empty());
    assert_eq!(service.carrier.live_watch_ids().count(), 0);
    // Exhaustion must terminate this attachment; an ID can never wrap/reuse.
    service.carrier.next_watch = u64::MAX;
    let mut exhausted = admitted(
        &owner,
        &current,
        &mut session,
        &action,
        false,
        |proof, view| {
            service.submit(
                proof,
                view,
                Instant::now() + std::time::Duration::from_secs(5),
            )
        },
    )
    .unwrap();
    let incoming = receiver.try_recv().unwrap();
    incoming.reply.send(Ok(accepted(host, invocation))).unwrap();
    drop(incoming._permit);
    let reply = exhausted.try_reply().unwrap().unwrap();
    assert!(
        service
            .observe_reply(&exhausted, &reply, current.read().unwrap().view())
            .is_err()
    );
    assert!(service.is_closed());
    assert!(service.fences.is_empty());
}
#[test]
fn native_private_retained_reply_expires_without_policy_document_change() {
    let (_dir, mut owner, current) = setup();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let expiry = now + 2;
    policy(
        &mut owner,
        &current,
        collab_types::caps::CapOverride::restrict_role_in_app(
            collab_types::Role::Owner,
            "so.tilo.word-processor",
            collab_types::caps::Verb::Read,
        )
        .expiring(collab_types::caps::Expiry::AtUnix(expiry)),
    );
    let generation = current.read().unwrap().view().generation();
    let mut session = session(&owner, &current);
    let (queue, mut receiver) = PeerActionQueue::channel();
    let host = Uuid::new_v4();
    let mut service = NativePrivateService {
        carrier: PrivateService::from_queue_unqualified(queue, host).unwrap(),
        clock: current.policy_clock().unwrap(),
        fences: HashMap::new(),
    };
    let invocation = tilo_actions::InvocationId(Uuid::new_v4());
    let action = action(&owner, ActionMessage::Status { invocation });
    let mut pending = admitted(
        &owner,
        &current,
        &mut session,
        &action,
        false,
        |proof, view| {
            service.submit(
                proof,
                view,
                Instant::now() + std::time::Duration::from_secs(5),
            )
        },
    )
    .unwrap();
    let incoming = receiver.try_recv().unwrap();
    incoming.reply.send(Ok(accepted(host, invocation))).unwrap();
    drop(incoming._permit);
    let reply = pending.try_reply().unwrap().unwrap();
    let boundary = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(expiry);
    std::thread::sleep(
        boundary
            .duration_since(std::time::SystemTime::now())
            .unwrap_or_default(),
    );
    assert_eq!(generation, current.read().unwrap().view().generation());
    context(&owner, &current, &session.1, |context, _| {
        session.0.revalidate(context).unwrap()
    });
    assert!(
        output(pending, current.read().unwrap().view(), &reply).is_err(),
        "AtUnix changed policy without a publication"
    );
}
