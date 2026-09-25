use super::*;
use crate::{ActionRequest, NativeOperation, Store};

fn fixture() -> (tempfile::TempDir, CloudStoreSource, [u8; 32]) {
    let (dir, mut store, device) = crate::agent_owner_tests::enrolled();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    let source = CloudStoreSource::bind(&store).unwrap();
    drop(store);
    (dir, source, device)
}
fn turn<R>(source: &CloudStoreSource, run: impl FnOnce(&CloudStoreTurn) -> R) -> R {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let mut run = Some(run);
    loop {
        match source.with_current(|turn| run.take().unwrap()(turn)) {
            Current::Current(value) => return value,
            Current::Busy => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "Store remains contended"
                );
                std::thread::yield_now();
            }
            Current::Denied => panic!("actual Store source denied"),
        }
    }
}
fn document(dir: &std::path::Path, device: [u8; 32], id: &str) {
    let mut store = Store::open(dir, "agent-admission").unwrap();
    store
        .action(
            device,
            &ActionRequest {
                version: 1,
                room: store.state.room.clone(),
                invocation_id: id.into(),
                operation: NativeOperation::WriterInsert {
                    text: "stable policy".into(),
                },
            },
            qos_relay::clock::now_ns(),
        )
        .unwrap();
}
#[test]
fn cloud_private_disclosure_same_store_origin_and_expiry_are_required() {
    let (dir, source, device) = fixture();
    let fence = turn(&source, |turn| {
        Disclosure::capture(&source, turn, turn.now_ns() + 2_000_000_000).unwrap()
    });
    assert!(turn(&source, |turn| fence.check(turn)).is_ok());
    document(dir.path(), device, "progress");
    std::thread::sleep(std::time::Duration::from_nanos(
        fence.expiry_ns.saturating_sub(qos_relay::clock::now_ns()),
    ));
    assert!(
        turn(&source, |turn| fence.check(turn)).is_err(),
        "captured expiry cannot survive Store reopen"
    );
}

// Public, test-only localhost TLS identity. Never production credentials.
const TLS_CERT: &str = "3082017d30820123a00302010202147ac297cf4de83a0525b286d5c7a90672c8fac5da300a06082a8648ce3d04030230143112301006035504030c096c6f63616c686f7374301e170d3236303932323039333131355a170d3336303931393039333131355a30143112301006035504030c096c6f63616c686f73743059301306072a8648ce3d020106082a8648ce3d0301070342000457ce6d80dfc27f71bc5c546417cf92a1621aa614a707057f2447ea54ede7034f0976988f9774dd07fa000cc12de7f304af8f32cb0ca315166245077b1b314655a3533051301d0603551d0e04160414d967e7ab5d6613dd0e53a648ff9b5d90657566f8301f0603551d23041830168014d967e7ab5d6613dd0e53a648ff9b5d90657566f8300f0603551d130101ff040530030101ff300a06082a8648ce3d0403020348003045022100985d69619df31e351a28d29da6c86ef8fb7d33ad87da5302228479f1614b4cf502203285805ff0e92e18155aab56f0229536582cee0c38653fca44e50f61cf7959f5";
const TLS_KEY: &str = "308187020100301306072a8648ce3d020106082a8648ce3d030107046d306b020101042058815db9251f1c7a1bbb3ccfd7ea7d5c0b0b680eac16f8bb2b09b60e06cc771da1440342000457ce6d80dfc27f71bc5c546417cf92a1621aa614a707057f2447ea54ede7034f0976988f9774dd07fa000cc12de7f304af8f32cb0ca315166245077b1b314655";

use ed25519_dalek::SigningKey;
use qos_relay::delivery_v3::{
    auth, http_proof,
    private_wire::{PrivateOperation, PrivateScope},
};
use qos_relay::peer_actions::ActionMessage;
use uuid::Uuid;
// Genuine proofs use the production authority on the actual Store turn.
// Descriptor/boot installation remains outside this component fixture.
fn crypto_context<R>(
    turn: &CloudStoreTurn,
    binding: &auth::Binding,
    run: impl FnOnce(&auth::VerificationContext<'_>) -> R,
) -> R {
    let revocations = qos_user_identity::RevocationList::new();
    let certificates = turn.peer().certificates();
    let device = certificates
        .iter()
        .find(|cert| cert.device_public_key == binding.device)
        .unwrap();
    let context = auth::VerificationContext {
        binding,
        capabilities: auth::CapabilityPolicy {
            supported: 6,
            allowed: 6,
        },
        credentials: auth::Credentials {
            device,
            host: turn.peer().authority(),
            device_revocations: &revocations,
            host_revocations: &revocations,
        },
        authority: turn,
        now_ns: turn.fresh_now_ns().unwrap(),
    };
    run(&context)
}
struct Session {
    admission: http_proof::HttpSessionAdmission,
    binding: auth::Binding,
    id: [u8; 16],
    digest: [u8; 32],
    key: SigningKey,
}
fn session(source: &CloudStoreSource, origin: &str) -> Session {
    turn(source, |turn| {
        let key = SigningKey::from_bytes(&[8; 32]);
        let certs = turn.peer().certificates();
        let device = certs
            .iter()
            .find(|cert| cert.device_public_key == key.verifying_key().to_bytes())
            .unwrap();
        let host = turn.peer().authority();
        let binding = auth::Binding {
            workspace: turn.workspace(),
            workspace_root: host.user_public_key,
            device: device.device_public_key,
            device_root: device.user_public_key,
            host: host.device_public_key,
            host_root: host.user_public_key,
            origin: origin.into(),
            descriptor_generation: 1,
            descriptor_digest: [8; 32],
            server_boot_id: [9; 16],
            carrier: auth::Carrier::Https,
            requested_capabilities: 6,
            mandatory_capabilities: 6,
        };
        let (admission, id, digest) = crypto_context(turn, &binding, |context| {
            let (challenge, server) =
                auth::Challenge::issue(context, [2; 32], &turn.store.key()).unwrap();
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
            .client_proof(&key)
            .unwrap();
            let verified = challenge.finish(&proof, context).unwrap();
            let id = verified.session_id();
            let digest = http_proof::session_binding_digest(
                verified.binding(),
                id,
                verified.capabilities(),
                verified.expires_at_ns(),
            )
            .unwrap();
            (
                http_proof::HttpSessionAdmission::new(verified, context).unwrap(),
                id,
                digest,
            )
        });
        Session {
            admission,
            binding,
            id,
            digest,
            key,
        }
    })
}
fn action(
    source: &CloudStoreSource,
    session: &Session,
    invocation: qos_actions::InvocationId,
) -> DirectedAction {
    turn(source, |turn| DirectedAction {
        room: turn.peer().room().parse().unwrap(),
        request_id: Uuid::new_v4(),
        source: session.binding.device,
        target: turn.peer().authority().device_public_key,
        message: ActionMessage::Status { invocation },
    })
}
fn operation(action: &DirectedAction) -> PrivateOperation {
    match action.message {
        ActionMessage::AgentReadV1 { .. } => PrivateOperation::AgentRead,
        ActionMessage::Status { .. } => PrivateOperation::Status,
        _ => panic!("fixture operation"),
    }
}
fn admitted<R>(
    source: &CloudStoreSource,
    session: &mut Session,
    action: &DirectedAction,
    poll: bool,
    run: impl FnOnce(VerifiedRequest<'_>, &CloudStoreTurn) -> R,
) -> R {
    let bytes = private_wire::encode_request(
        operation(action),
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
        http_proof::Request::new("POST", operation(action).path(), &bytes).unwrap()
    };
    turn(source, |turn| {
        crypto_context(turn, &session.binding, |context| {
            let proof = http_proof::RequestProof::sign(
                session.id,
                session.digest,
                &exact,
                context.now_ns,
                context.now_ns + 1_000_000_000,
                &session.key,
            )
            .unwrap();
            run(
                session.admission.admit(&proof, &exact, context).unwrap(),
                turn,
            )
        })
    })
}
fn policy(dir: &std::path::Path, cap: collab_types::caps::CapOverride) {
    use automerge::{ROOT, ReadDoc, ScalarValue, transaction::Transactable};
    let mut store = Store::open(dir, "agent-admission").unwrap();
    let peer = store.peer().unwrap();
    let mut doc = peer.admitted_room().unwrap().clone();
    doc.set_actor(automerge::ActorId::from(
        store.key().verifying_key().to_bytes().to_vec(),
    ));
    let caps = doc.get(ROOT, "capabilities").unwrap().unwrap().1;
    doc.put(
        &caps,
        "private-output-test",
        ScalarValue::Bytes(serde_json::to_vec(&cap).unwrap()),
    )
    .unwrap();
    doc.commit();
    store
        .receive(
            qos_peer_state::TransportOrigin::RelayDelta(store.key().verifying_key().to_bytes()),
            qos_peer_state::NeighbourMsg::SyncResp {
                room: store.state.room.clone(),
                changes: doc
                    .get_changes(&[])
                    .into_iter()
                    .map(|c| c.raw_bytes().to_vec())
                    .collect(),
            },
            qos_relay::clock::now_ns(),
        )
        .unwrap();
}
fn cap(restrict: bool) -> collab_types::caps::CapOverride {
    use collab_types::caps::{CapOverride, Effect, Scope, Subject, Verb};
    CapOverride::new(
        Subject::Role(collab_types::Role::Owner),
        Scope::App("so.tilo.word-processor".into()),
        if restrict {
            Effect::Restrict
        } else {
            Effect::Grant
        },
        if restrict { Verb::Read } else { Verb::Admin },
    )
}
fn write_allowed(source: &CloudStoreSource) -> bool {
    turn(source, |turn| {
        use collab_types::caps::{Actor, CapDecision, ResourceRef, Verb, decide_capability};
        let (_, owner, caps) =
            qos_peer_state::room::extract_governed_fields(turn.peer().admitted_room().unwrap());
        let principal = hex::encode(turn.peer().authority().user_public_key);
        let now = turn.fresh_now_ns().unwrap() / 1_000_000_000;
        let actor = Actor::person(
            qos_peer_state::room::role_of(owner.as_deref(), &caps, &principal, now),
            principal,
        )
        .in_room(turn.peer().room());
        decide_capability(
            &actor,
            &ResourceRef::Action {
                app: "so.tilo.word-processor".into(),
                action: "replace-text".into(),
            },
            Verb::Write,
            &caps,
            Some(now),
        ) == CapDecision::Allow
    })
}
fn accepted(host: Uuid, invocation: qos_actions::InvocationId) -> ActionMessage {
    ActionMessage::Observation {
        host,
        invocation,
        update: Some(qos_actions::InvocationUpdate::Accepted { invocation }),
        uncertain: false,
        cancellation_requested: false,
    }
}
fn service(source: &CloudStoreSource, queue: PeerActionQueue) -> CloudPrivateService {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        match CloudPrivateService::new_unqualified(source, &queue) {
            Current::Current(Ok(value)) => return value,
            Current::Current(Err(error)) => panic!("actual Store carrier: {error}"),
            Current::Denied => panic!("actual Store source denied"),
            Current::Busy => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "carrier Store remains contended"
                );
                std::thread::yield_now();
            }
        }
    }
}

struct HttpFixture {
    _assets: tempfile::TempDir,
    origin: String,
    receiver: qos_session_peer::https::IngressReceiver,
    stop: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}
impl HttpFixture {
    async fn start() -> Self {
        let assets = tempfile::tempdir().unwrap();
        std::fs::write(assets.path().join("index.html"), b"fixture").unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!(
            "https://localhost:{}",
            listener.local_addr().unwrap().port()
        );
        let (ingress, receiver) = qos_session_peer::https::try_ingress_channel().unwrap();
        let adapter = qos_session_peer::https::HttpsAdapter::new(
            listener,
            &origin,
            qos_session_peer::https_assets::AssetRoot::open(assets.path()).unwrap(),
            vec![hex::decode(TLS_CERT).unwrap().into()],
            hex::decode(TLS_KEY).unwrap().try_into().unwrap(),
            ingress,
        )
        .unwrap();
        let (stop, rx) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(adapter.serve(rx));
        Self {
            _assets: assets,
            origin,
            receiver,
            stop,
            task,
        }
    }
    async fn request(
        &mut self,
        source: &CloudStoreSource,
        session: &Session,
        action: &DirectedAction,
    ) -> (BufferedRequest, tokio::task::JoinHandle<reqwest::Response>) {
        let bytes = private_wire::encode_request(
            operation(action),
            PrivateScope {
                room: action.room,
                source: action.source,
                target: action.target,
            },
            action,
        )
        .unwrap();
        let exact = http_proof::Request::new("POST", operation(action).path(), &bytes).unwrap();
        let proof = turn(source, |turn| {
            http_proof::RequestProof::sign(
                session.id,
                session.digest,
                &exact,
                turn.now_ns(),
                turn.now_ns() + 1_000_000_000,
                &session.key,
            )
            .unwrap()
        });
        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap();
        let url = format!("{}{}", self.origin, operation(action).path());
        let task = tokio::spawn(async move {
            client
                .post(url)
                .header(
                    "x-tilo-delivery-proof",
                    hex::encode(proof.encode().unwrap()),
                )
                .body(bytes)
                .send()
                .await
                .unwrap()
        });
        let request = tokio::time::timeout(std::time::Duration::from_secs(3), self.receiver.recv())
            .await
            .unwrap()
            .unwrap();
        (request, task)
    }
    async fn finish(self) {
        let _ = self.stop.send(true);
        self.task.await.unwrap().unwrap();
    }
}

#[test]
fn cloud_private_actual_external_writer_aba_fences_equal_policy_and_keeps_progress() {
    let (dir, source, device) = fixture();
    policy(dir.path(), cap(false));
    assert!(write_allowed(&source));
    let (fence, before) = turn(&source, |turn| {
        (
            Disclosure::capture(&source, turn, u64::MAX).unwrap(),
            turn.generation(),
        )
    });
    document(dir.path(), device, "policy-stable-progress");
    turn(&source, |turn| {
        assert!(turn.generation() > before);
        assert!(fence.check(turn).is_ok());
    });
    policy(dir.path(), cap(true));
    assert!(!write_allowed(&source));
    assert!(turn(&source, |turn| fence.check(turn)).is_err());
    policy(dir.path(), cap(false));
    turn(&source, |turn| {
        assert_eq!(
            fence.revision,
            PrivatePolicyBasis::from_peer(turn.peer())
                .unwrap()
                .revision()
        );
        assert!(turn.policy_lineage() > fence.lineage);
        assert!(
            fence.check(turn).is_err(),
            "equal policy after external A-B-A cannot revive output"
        );
    });
    let store = Store::open(dir.path(), "agent-admission").unwrap();
    let another = CloudStoreSource::bind(&store).unwrap();
    drop(store);
    let fresh = turn(&source, |turn| {
        Disclosure::capture(&source, turn, u64::MAX).unwrap()
    });
    assert!(
        turn(&another, |turn| fresh.check(turn)).is_err(),
        "new source cannot adopt another lifetime fence"
    );
}
#[tokio::test(flavor = "current_thread")]
async fn cloud_private_actual_http_busy_preserves_original_custody_and_stale_output_is_withheld() {
    let (dir, source, device) = fixture();
    let mut http = HttpFixture::start().await;
    let mut session = session(&source, &http.origin);
    let (queue, mut receiver) = PeerActionQueue::channel();
    let mut service = service(&source, queue.clone());
    let invocation = qos_actions::InvocationId(Uuid::new_v4());
    let action = action(&source, &session, invocation);
    let host = action.room;
    let (request, client) = http.request(&source, &session, &action).await;
    // Verify the exact actual buffered HTTP body/proof, not a parallel request.
    let proof = http_proof::RequestProof::decode(request.proof().unwrap()).unwrap();
    let exact =
        http_proof::Request::new(request.method().as_str(), request.target(), request.body())
            .unwrap();
    let pending = turn(&source, |turn| {
        crypto_context(turn, &session.binding, |context| {
            service.submit(
                session.admission.admit(&proof, &exact, context).unwrap(),
                turn,
                request.deadline(),
            )
        })
    })
    .unwrap();
    let incoming = receiver.try_recv().unwrap();
    let original_time = turn(&source, |turn| turn.policy_time().unwrap());
    assert!(
        incoming
            .policy_time()
            .is_some_and(|time| time.same_origin(&original_time)),
        "actual queued request must retain the original Store Source time"
    );
    assert_eq!(incoming.deadline, request.deadline());
    incoming.reply.send(Ok(accepted(host, invocation))).unwrap();
    drop(incoming._permit);
    let mut pending = Some(pending);
    let reply = pending.as_mut().unwrap().try_reply().unwrap().unwrap();
    let mut request = Some(request);
    let mut credits = Some(ResponseCredits::default());
    let busy = Store::open(dir.path(), "agent-admission").unwrap();
    assert!(matches!(
        CloudPendingPrivate::respond(&mut pending, &reply, &mut request, &mut credits),
        Current::Busy
    ));
    assert!(pending.is_some() && request.is_some() && credits.is_some());
    assert!(!client.is_finished());
    drop(busy);
    document(dir.path(), device, "http-stable-progress");
    assert!(matches!(
        CloudPendingPrivate::respond(&mut pending, &reply, &mut request, &mut credits),
        Current::Current(Ok(()))
    ));
    assert!(pending.is_none() && request.is_none() && credits.is_none());
    let response = client.await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body = response.bytes().await.unwrap();
    assert_eq!(
        private_wire::decode_response(PrivateOperation::Status, &action, &body).unwrap(),
        reply
    );

    let (request, client) = http.request(&source, &session, &action).await;
    let proof = http_proof::RequestProof::decode(request.proof().unwrap()).unwrap();
    let exact =
        http_proof::Request::new(request.method().as_str(), request.target(), request.body())
            .unwrap();
    let mut retained = turn(&source, |turn| {
        crypto_context(turn, &session.binding, |context| {
            service.submit(
                session.admission.admit(&proof, &exact, context).unwrap(),
                turn,
                request.deadline(),
            )
        })
    })
    .unwrap();
    let incoming = receiver.try_recv().unwrap();
    incoming.reply.send(Ok(accepted(host, invocation))).unwrap();
    drop(incoming._permit);
    let reply = retained.try_reply().unwrap().unwrap();
    policy(dir.path(), cap(true));
    turn(&source, |turn| {
        crypto_context(turn, &session.binding, |context| {
            session.admission.revalidate(context).unwrap()
        })
    });
    let mut pending = Some(retained);
    let mut request = Some(request);
    let mut credits = Some(ResponseCredits::default());
    assert!(
        matches!(
            CloudPendingPrivate::respond(&mut pending, &reply, &mut request, &mut credits),
            Current::Current(Err(_))
        ),
        "actual buffered private result escaped action policy change"
    );
    assert!(pending.is_some() && request.is_some() && credits.is_some());
    assert!(!client.is_finished());
    request
        .take()
        .unwrap()
        .respond(reqwest::StatusCode::FORBIDDEN, vec![])
        .unwrap();
    drop(pending);
    drop(credits);
    let response = client.await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    assert!(response.bytes().await.unwrap().is_empty());
    service.close();
    http.finish().await;
}

#[test]
fn cloud_private_same_uuid_status_does_not_refresh_older_ready_event_fence() {
    // Controlled broker notifications, actual signed admission + Store policy.
    // This does not simulate or qualify actual agent/worker effect execution.
    let (dir, source, _) = fixture();
    let mut session = session(&source, "https://localhost:9443");
    let (queue, mut receiver) = PeerActionQueue::channel();
    let lock = Store::open(dir.path(), "agent-admission").unwrap();
    assert!(matches!(
        CloudPrivateService::new_unqualified(&source, &queue),
        Current::Busy
    ));
    assert!(!queue.is_closed());
    drop(lock);
    let mut service = service(&source, queue.clone());
    let invocation = qos_actions::InvocationId(Uuid::new_v4());
    let action = action(&source, &session, invocation);
    let host = action.room;
    let mut initial = admitted(&source, &mut session, &action, false, |proof, turn| {
        service.submit(
            proof,
            turn,
            Instant::now() + std::time::Duration::from_secs(5),
        )
    })
    .unwrap();
    let incoming = receiver.try_recv().unwrap();
    let evidence = incoming.evidence.unwrap();
    incoming.reply.send(Ok(accepted(host, invocation))).unwrap();
    drop(incoming._permit);
    let reply = initial.try_reply().unwrap().unwrap();
    turn(&source, |turn| {
        service.observe_reply(&initial, &reply, turn)
    })
    .unwrap();
    drop(initial);
    let original = *service.fences.keys().next().unwrap();
    let progress = qos_actions::InvocationUpdate::Progress {
        invocation,
        sequence: 1,
        phase: "running".into(),
        completed: None,
        total: None,
        message: Some("private transition".into()),
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
    policy(dir.path(), cap(true));
    let mut fresh = admitted(&source, &mut session, &action, false, |proof, turn| {
        service.submit(
            proof,
            turn,
            Instant::now() + std::time::Duration::from_secs(5),
        )
    })
    .unwrap();
    let incoming = receiver.try_recv().unwrap();
    incoming.reply.send(Ok(accepted(host, invocation))).unwrap();
    drop(incoming._permit);
    let reply = fresh.try_reply().unwrap().unwrap();
    turn(&source, |turn| service.observe_reply(&fresh, &reply, turn)).unwrap();
    assert_eq!(service.fences.len(), 1);
    assert!(service.fences.contains_key(&original));
    assert!(
        admitted(&source, &mut session, &action, true, |proof, turn| service
            .poll_events(proof, turn))
        .is_err(),
        "later same-UUID Status blessed older policy's ready event"
    );
    service.retire_session(session.id);
    assert!(service.fences.is_empty());
    service.close();
    assert!(service.is_closed());
    assert!(fresh.try_reply().is_err());
}
#[test]
fn cloud_private_at_unix_rechecked_within_same_actual_store_turn() {
    let (dir, source, _) = fixture();
    let expiry = qos_relay::clock::now_ns() / 1_000_000_000 + 2;
    policy(
        dir.path(),
        cap(true).expiring(collab_types::caps::Expiry::AtUnix(expiry)),
    );
    let fence = turn(&source, |turn| {
        Disclosure::capture(&source, turn, u64::MAX).unwrap()
    });
    assert_eq!(fence.expiry_ns, expiry * 1_000_000_000);
    turn(&source, |turn| {
        let generation = turn.generation();
        assert!(fence.check(turn).is_ok());
        assert!(
            matches!(
                crate::delivery::access::try_open(dir.path(), "agent-admission"),
                Err(crate::delivery::access::AccessError::Busy)
            ),
            "actual Store lock held throughout disclosure"
        );
        std::thread::sleep(std::time::Duration::from_nanos(
            fence.expiry_ns.saturating_sub(qos_relay::clock::now_ns()),
        ));
        assert_eq!(generation, turn.generation());
        assert!(
            turn.now_ns() < fence.expiry_ns,
            "admission timestamp intentionally stable"
        );
        assert!(
            fence.check(turn).is_err(),
            "stable admission time must not extend policy across encoding/output"
        );
    });
}
#[test]
fn cloud_private_corrupt_source_cannot_revive_after_file_restore() {
    let (dir, source, _) = fixture();
    let fence = turn(&source, |turn| {
        Disclosure::capture(&source, turn, u64::MAX).unwrap()
    });
    let path = dir.path().join("state.json");
    let original = std::fs::read(&path).unwrap();
    std::fs::write(&path, b"broken").unwrap();
    assert!(matches!(
        source.with_current(|turn| fence.check(turn)),
        Current::Denied
    ));
    std::fs::write(&path, &original).unwrap();
    assert!(matches!(
        source.with_current(|turn| fence.check(turn)),
        Current::Denied
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn cloud_private_actual_read_return_keeps_original_custody_through_http_handoff() {
    use qos_action_broker::agent_reads::AgentReadCustody;
    use qos_action_broker::peer_actions::CarrierError;
    // Explicitly select the existing private schema before V6; an unselected
    // private ledger correctly returns NotConfigured rather than a fake list.
    let (dir, mut store, device) = crate::agent_owner_tests::enrolled();
    store
        .activate_private_agent_ledger(&store.agent_caller(device).unwrap())
        .unwrap();
    let id = Uuid::new_v4();
    let (caller, prepared) = crate::agent_owner_tests::prepare(&store, device, &id.to_string());
    store.commit_agent_request(&prepared, &caller).unwrap();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    let clock = qos_action_broker::peer_actions::OwnerPolicyClock::new().unwrap();
    let source = CloudStoreSource::bind_with_clock(&store, clock.clone()).unwrap();
    drop(store);
    let source_time = turn(&source, |turn| turn.policy_time().unwrap());
    let mut runtime = crate::agent_runtime::CloudAgentRuntime::start_with_policy_time(
        dir.path(),
        "agent-admission",
        true,
        clock,
        source_time.clone(),
        source.clone(),
    )
    .await
    .unwrap();
    let commands = runtime.certified_commands();
    let mut http = HttpFixture::start().await;
    let mut session = session(&source, &http.origin);
    let (queue, mut receiver) = PeerActionQueue::channel();
    let mut service = service(&source, queue.clone());
    let mut action = action(&source, &session, qos_actions::InvocationId(Uuid::new_v4()));
    action.message = ActionMessage::AgentReadV1 {
        host: action.room,
        invocation: Uuid::new_v4(),
        request: qos_agent_control_protocol::AgentRequest::List {
            version: 1,
            cursor: None,
        },
    };
    // A debug-build owner read under heavy parallel test load can miss its
    // 5 s request deadline; that is retried with a fresh original request.
    // Every custody assertion below applies to the attempt that completed.
    let mut attempts = 0;
    let (request, client, mut pending, incoming, returned) = loop {
        attempts += 1;
        if attempts > 1 {
            action.request_id = Uuid::new_v4();
            if let ActionMessage::AgentReadV1 { invocation, .. } = &mut action.message {
                *invocation = Uuid::new_v4();
            }
        }
        let (request, client) = http.request(&source, &session, &action).await;
        let proof = http_proof::RequestProof::decode(request.proof().unwrap()).unwrap();
        let exact =
            http_proof::Request::new(request.method().as_str(), request.target(), request.body())
                .unwrap();
        let pending = turn(&source, |turn| {
            crypto_context(turn, &session.binding, |context| {
                service.submit(
                    session.admission.admit(&proof, &exact, context).unwrap(),
                    turn,
                    request.deadline(),
                )
            })
        })
        .unwrap();
        let incoming = receiver.try_recv().unwrap();
        assert!(
            incoming
                .policy_time()
                .is_some_and(|time| time.same_origin(&source_time))
        );
        let custody = AgentReadCustody::from_request(&incoming);
        let evidence = incoming.evidence.as_ref().unwrap().clone();
        let deadline = incoming.deadline;
        match commands
            .handle_certified_with_custody(evidence, deadline, custody)
            .await
        {
            Ok(reply) => break (request, client, pending, incoming, reply),
            Err(error) if attempts < 3 && error.contains("deadline") => {
                eprintln!("owner read attempt {attempts} missed its deadline; retrying");
                drop(incoming);
                drop(pending);
                drop(request);
                client.abort();
            }
            Err(error) => {
                let stopped = runtime.close().await;
                panic!("actual owner read failed: {error}; runtime result: {stopped:?}");
            }
        }
    };
    let evidence = incoming.evidence.as_ref().unwrap().clone();
    let deadline = incoming.deadline;
    let (message, agent_read) = returned.into_parts();
    assert!(agent_read.is_some());
    let message = message.unwrap();
    let ActionMessage::AgentReadResultV1 {
        response: qos_agent_control_protocol::AgentResponse::List { agents, .. },
        ..
    } = &message
    else {
        panic!("actual selected Store list missing: {message:?}")
    };
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].id, format!("agent:{id}"));
    incoming.reply.send(Ok(message)).unwrap();
    drop(incoming._permit);
    let reply = pending.try_reply().unwrap().unwrap();
    let mut held = Vec::new();
    for _ in 0..31 {
        held.push(queue.submit_certified(evidence.clone(), deadline).unwrap());
    }
    assert!(matches!(
        queue.submit_certified(evidence.clone(), deadline),
        Err(CarrierError::Capacity)
    ));
    let mut pending = Some(pending);
    let mut request = Some(request);
    let mut credits = Some(ResponseCredits {
        agent_read,
        ..Default::default()
    });
    // Exact actual Store comparison and final response handoff occur together.
    loop {
        match CloudPendingPrivate::respond(&mut pending, &reply, &mut request, &mut credits) {
            Current::Current(result) => {
                result.unwrap();
                break;
            }
            Current::Denied => panic!("read output source denied"),
            Current::Busy => {
                assert!(Instant::now() < deadline);
                assert!(pending.is_some() && request.is_some() && credits.is_some());
                tokio::task::yield_now().await;
            }
        }
    }
    assert!(pending.is_none() && request.is_none() && credits.is_none());
    let response = client.await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body = response.bytes().await.unwrap();
    assert_eq!(
        private_wire::decode_response(PrivateOperation::AgentRead, &action, &body).unwrap(),
        reply
    );
    // Client Bytes are a network copy, not the server backing; backing clone/
    // slice custody is separately tested in the transport crate itself.
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match queue.submit_certified(evidence.clone(), deadline) {
                Ok(pending) => {
                    drop(pending);
                    break;
                }
                Err(CarrierError::Capacity) => tokio::task::yield_now().await,
                Err(error) => panic!("unexpected queue error: {error}"),
            }
        }
    })
    .await
    .unwrap();
    drop(held);
    service.close();
    http.finish().await;
    runtime.close().await.unwrap();
}

/// Scripted durable status source standing in for the agent owner's Store
/// reads; everything else (queue, carrier watch, disclosure fence) is actual.
/// Answers `active` until the test durably "commits" `terminal`.
struct Scripted {
    active: ActionMessage,
    terminal: ActionMessage,
    committed: std::sync::atomic::AtomicBool,
    calls: std::sync::atomic::AtomicUsize,
}
impl crate::https_owner::service::ObserveInvocation for std::sync::Arc<Scripted> {
    fn observe(
        &self,
        _evidence: qos_relay::client::CertifiedAction,
        _invocation: qos_actions::InvocationId,
        _deadline: Instant,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<ActionMessage>, String>> + Send + '_>,
    > {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let next = if self.committed.load(std::sync::atomic::Ordering::SeqCst) {
            self.terminal.clone()
        } else {
            self.active.clone()
        };
        Box::pin(async move { Ok(Some(next)) })
    }
}

/// A watched active invocation whose durable status changes after a Store
/// commit is published once as a streaming transition; the carrier turns it
/// into a fresh authorized notification request and the session's
/// `/private/events` poll receives it. Terminal status ends observation.
#[tokio::test]
async fn cloud_private_committed_status_change_streams_to_the_watching_session() {
    let (_dir, source, _) = fixture();
    let mut session = session(&source, "https://localhost:9443");
    let (queue, mut receiver) = PeerActionQueue::channel();
    let mut service = service(&source, queue.clone());
    let invocation = qos_actions::InvocationId(Uuid::new_v4());
    let action = action(&source, &session, invocation);
    let host = action.room;
    let mut initial = admitted(&source, &mut session, &action, false, |proof, turn| {
        service.submit(
            proof,
            turn,
            Instant::now() + std::time::Duration::from_secs(5),
        )
    })
    .unwrap();
    let incoming = receiver.try_recv().unwrap();
    let evidence = incoming.evidence.clone().unwrap();
    incoming.reply.send(Ok(accepted(host, invocation))).unwrap();
    drop(incoming._permit);
    let reply = initial.try_reply().unwrap().unwrap();
    turn(&source, |turn| {
        service.observe_reply(&initial, &reply, turn)
    })
    .unwrap();
    drop(initial);
    assert_eq!(service.fences.len(), 1, "active status installs a watch");

    let succeeded = ActionMessage::Observation {
        host,
        invocation,
        update: Some(qos_actions::InvocationUpdate::Succeeded {
            invocation,
            output: qos_actions::ActionOutput {
                value: Some(serde_json::json!({"done": true})),
                resources: vec![],
            },
        }),
        uncertain: false,
        cancellation_requested: false,
    };
    let script = std::sync::Arc::new(Scripted {
        active: accepted(host, invocation),
        terminal: succeeded.clone(),
        committed: Default::default(),
        calls: Default::default(),
    });
    let (register, registrations) = tokio::sync::mpsc::channel(4);
    let (commits, committed) = tokio::sync::watch::channel(0u64);
    let observer = tokio::spawn(crate::https_owner::service::observe_transitions(
        script.clone(),
        queue.clone(),
        registrations,
        committed,
        || {},
    ));
    register
        .send(crate::https_owner::service::Watched {
            evidence,
            invocation,
            last: accepted(host, invocation),
            until: Instant::now() + std::time::Duration::from_secs(60),
        })
        .await
        .unwrap();
    // First commit: status unchanged, nothing published.
    commits.send_modify(|n| *n += 1);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    service.pump().unwrap();
    assert!(
        receiver.try_recv().is_err(),
        "unchanged status is not a transition"
    );
    // Second commit: the durable status became terminal.
    script
        .committed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    commits.send_modify(|n| *n += 1);
    let notification = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            service.pump().unwrap();
            if let Ok(incoming) = receiver.try_recv() {
                break incoming;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("published transition reaches the watching carrier");
    assert_eq!(notification.poll, Some(invocation));
    let ActionMessage::Observation {
        update: Some(update),
        ..
    } = &succeeded
    else {
        unreachable!()
    };
    assert_eq!(notification.notification.as_ref(), Some(update));
    notification.reply.send(Ok(succeeded.clone())).unwrap();
    drop(notification._permit);
    service.pump().unwrap();
    let event = admitted(&source, &mut session, &action, true, |proof, turn| {
        service.poll_events(proof, turn)
    })
    .unwrap()
    .expect("ready streaming event");
    assert_eq!(event.1.message, succeeded);
    // Terminal status ended observation: later commits observe nothing more.
    let calls = script.calls.load(std::sync::atomic::Ordering::SeqCst);
    commits.send_modify(|n| *n += 1);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(
        script.calls.load(std::sync::atomic::Ordering::SeqCst),
        calls
    );
    observer.abort();
    service.close();
}

/// The real agent owner answers a watch observation from its retained Store
/// status for the evidence's own invocation, and refuses another invocation.
#[tokio::test]
async fn cloud_private_actual_agent_owner_observes_the_evidence_invocation_only() {
    let (dir, mut store, device) = crate::agent_owner_tests::enrolled();
    store
        .activate_private_agent_ledger(&store.agent_caller(device).unwrap())
        .unwrap();
    let id = Uuid::new_v4();
    let (caller, prepared) = crate::agent_owner_tests::prepare(&store, device, &id.to_string());
    store.commit_agent_request(&prepared, &caller).unwrap();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    let clock = qos_action_broker::peer_actions::OwnerPolicyClock::new().unwrap();
    let source = CloudStoreSource::bind_with_clock(&store, clock.clone()).unwrap();
    drop(store);
    let source_time = turn(&source, |turn| turn.policy_time().unwrap());
    let mut runtime = crate::agent_runtime::CloudAgentRuntime::start_with_policy_time(
        dir.path(),
        "agent-admission",
        true,
        clock,
        source_time,
        source.clone(),
    )
    .await
    .unwrap();
    let commands = runtime.certified_commands();
    let mut session = session(&source, "https://localhost:9443");
    let (queue, mut receiver) = PeerActionQueue::channel();
    let mut service = service(&source, queue.clone());
    let invocation = qos_actions::InvocationId(id);
    let action = action(&source, &session, invocation);
    let _pending = admitted(&source, &mut session, &action, false, |proof, turn| {
        service.submit(
            proof,
            turn,
            Instant::now() + std::time::Duration::from_secs(5),
        )
    })
    .unwrap();
    let evidence = receiver.try_recv().unwrap().evidence.unwrap();
    // Each owner command has a 5 s deadline; a freshly started runtime under a
    // loaded test process may miss one, so retry this read-only observation.
    let started = Instant::now();
    let observed = loop {
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        match commands
            .observe_certified(evidence.clone(), invocation, deadline)
            .await
        {
            Ok(observed) => break observed.expect("reserved agent invocation"),
            Err(error) if started.elapsed() < std::time::Duration::from_secs(30) => {
                eprintln!("observation retry: {error}");
            }
            Err(error) => panic!("actual owner observation failed: {error}"),
        }
    };
    let deadline = Instant::now() + std::time::Duration::from_secs(5);
    assert!(matches!(
        observed,
        ActionMessage::Observation { invocation: observed, .. } if observed == invocation
    ));
    assert!(
        commands
            .observe_certified(
                evidence,
                qos_actions::InvocationId(Uuid::new_v4()),
                deadline
            )
            .await
            .is_err()
    );
    service.close();
    runtime.close().await.unwrap();
}
