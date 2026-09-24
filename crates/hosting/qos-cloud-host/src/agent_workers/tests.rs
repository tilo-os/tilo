use super::*;
use qos_actions::Action;

pub(super) fn selected() -> (tempfile::TempDir, Arc<RuntimeOwner>) {
    let (directory, mut store, device) = crate::agent_owner_tests::enrolled();
    let mut peer = store.peer().unwrap();
    peer.select_admission_v5().unwrap();
    let mut state = store.state.clone();
    state.bound.as_mut().unwrap().peer = peer.durable().unwrap();
    store.commit(state).unwrap();
    // Retain a real preexisting private preparation through explicit V2
    // selection. Worker documents must never replace these unknown facts.
    let caller = store.agent_caller(device).unwrap();
    store.activate_private_agent_ledger(&caller).unwrap();
    let (caller, prior) =
        crate::agent_owner_tests::prepare(&store, device, "preexisting-worker-fact");
    store.commit_agent_request(&prior, &caller).unwrap();
    store.activate_private_agent_runtime_v2(&caller).unwrap();
    drop(store);
    let owner = Arc::new(RuntimeOwner::open(directory.path(), "agent-admission").unwrap());
    (directory, owner)
}

// A second host/owner or a legacy/no-owner broker must not stand in for the
// actual Cloud owner-bound broker used by the managed runtime.
#[tokio::test(flavor = "current_thread")]
async fn cloud_workers_start_real_certified_broker() {
    let (directory, owner) = selected();
    let (outbound, _network) = tokio::sync::mpsc::channel(32);
    let (mut workers, _documents) = AgentWorkers::start(directory.path(), owner, outbound)
        .expect("real enrolled Cloud worker assembly");
    assert!(workers.app_broker().is_ok());
    workers.close().unwrap();
}

const WRITER: &str = "so.tilo.word-processor";
const RECORD: &str = "tab:0000000000000001";

fn current_document(owner: &RuntimeOwner) -> Vec<u8> {
    owner
        .store()
        .unwrap()
        .peer()
        .unwrap()
        .document(WRITER, RECORD)
        .unwrap()
}

fn splice(document: &mut ui::automerge::AutoCommit, text: &str) {
    let model = qdoc::model::open(document, &ui::automerge::ROOT).unwrap();
    let block = qdoc::model::blocks_in_order(document, &model)[0].clone();
    word_processor::actions::WordProcessorAction::SpliceText {
        region: word_processor::actions::RegionKind::Body,
        block,
        at: 0,
        del: 0,
        ins: text.into(),
    }
    .apply(document)
    .unwrap();
    document.commit();
}

fn apply_document(workers: &mut AgentWorkers, pending: PendingHostDocument) {
    let owner = workers.owner.clone();
    let (complete, outcome) = crate::agent_documents::apply_worker_document(
        &owner,
        pending,
        std::time::Instant::now() + std::time::Duration::from_secs(5),
    );
    outcome.unwrap();
    // Opening Store here and in committed()/health() proves that the callback
    // released its exclusive Store lock before deferred worker completion.
    let _ = current_document(&owner);
    workers.committed().unwrap();
    complete();
}

async fn live_workers() -> (
    tempfile::TempDir,
    AgentWorkers,
    mpsc::Receiver<PendingHostDocument>,
    mpsc::Receiver<NeighbourMsg>,
) {
    let (directory, owner) = selected();
    let (outbound, mut network) = mpsc::channel(32);
    let (mut workers, mut documents) =
        AgentWorkers::start(directory.path(), owner, outbound).unwrap();
    let initial = workers.publications().unwrap();
    assert!(
        !initial.ready && initial.grants.is_empty(),
        "transport has not registered"
    );
    workers.set_transport_ready(true);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        workers.pump().unwrap();
        while let Ok(pending) = documents.try_recv() {
            apply_document(&mut workers, pending);
        }
        while network.try_recv().is_ok() {}
        if workers.publications().unwrap().ready
            && workers
                .broker
                .published_action_surfaces()
                .iter()
                .any(|surface| {
                    surface.live
                        && surface.app_id == WRITER
                        && surface.route.record_id == RECORD
                        && surface
                            .actions
                            .iter()
                            .any(|action| action.action == "insert-table")
                })
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "real worker did not publish its actions"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    (directory, workers, documents, network)
}

/// A certified device caller (the enrolled owner's browser device) invoking
/// through the worker broker's actual peer-action queue. Only this kind of
/// original caller yields custody the final scoped document evaluator accepts;
/// a host-internal `AppActionBroker::start` has no original caller at all.
pub(super) struct CertifiedJob {
    pub(super) id: u64,
    _pending: qos_action_broker::peer_actions::PendingPeerAction,
}

async fn certified_start(
    workers: &mut AgentWorkers,
    documents: &mut mpsc::Receiver<PendingHostDocument>,
    network: &mut mpsc::Receiver<NeighbourMsg>,
    surface: &qos_action_broker::collab_broker_capnp::PublishedSurface,
    resource: &str,
    action: &str,
) -> (AppActionBroker, CertifiedJob) {
    use qos_relay::peer_actions::ActionMessage;
    let invocation = qos_actions::InvocationId::new();
    let (queue, host) = workers
        .app_broker()
        .unwrap()
        .peer_action_attachment()
        .unwrap();
    let evidence = {
        let store = workers.owner.store().unwrap();
        crate::agent_runtime::read_attachment::tests::certified(
            &store,
            ActionMessage::InvokeBound {
                host,
                invocation,
                app_peer: surface.peer_id.to_string(),
                app_id: surface.app_id.clone(),
                record_id: surface.route.record_id.clone(),
                resource: resource.into(),
                action: action.into(),
                argument: None,
                confirmation: None,
                revision: None,
            },
        )
    };
    let mut pending = queue
        .submit_certified_with_policy_time(
            evidence,
            tokio::time::Instant::now() + std::time::Duration::from_secs(20),
            workers.owner.policy_time().unwrap(),
        )
        .unwrap();
    let broker = workers.app_broker().unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut replied = false;
    loop {
        // Admission is the broker thread's job. Leave worker pumping and any
        // resulting document handoff to the caller's own loop.
        let _ = &documents;
        while network.try_recv().is_ok() {}
        // Certified jobs are peer-owned (no local actor listing); wire ids are
        // allocated sequentially from the broker journal.
        if let Some(id) = (0..4096_u64).find(|id| {
            broker
                .status(*id)
                .is_some_and(|status| status.invocation() == invocation)
        }) {
            return (
                broker,
                CertifiedJob {
                    id,
                    _pending: pending,
                },
            );
        }
        if !replied {
            match pending.try_reply() {
                Ok(None) => {}
                Ok(Some(reply))
                    if matches!(
                        reply.message,
                        qos_relay::peer_actions::ActionMessage::Observation { .. }
                    ) =>
                {
                    replied = true;
                }
                reply => panic!("certified invocation refused: {reply:?}"),
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "certified invocation was not admitted"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

async fn start_table(
    workers: &mut AgentWorkers,
    documents: &mut mpsc::Receiver<PendingHostDocument>,
    network: &mut mpsc::Receiver<NeighbourMsg>,
) -> (AppActionBroker, CertifiedJob) {
    let (surface, action) = workers
        .broker
        .published_action_surfaces()
        .into_iter()
        .filter(|surface| {
            surface.live
                && surface.app_id == WRITER
                && surface.route.record_id == RECORD
                && surface.route.cluster_id == workers.room
        })
        .find_map(|surface| {
            surface
                .actions
                .iter()
                .find(|action| action.action == "insert-table")
                .cloned()
                .map(|action| (surface, action))
        })
        .unwrap();
    certified_start(
        workers,
        documents,
        network,
        &surface,
        &action.resource,
        &action.action,
    )
    .await
}

#[tokio::test(flavor = "current_thread")]
async fn cloud_worker_actual_action_waits_for_store_and_consumes_exact_receipt_once() {
    let (directory, mut workers, mut documents, mut network) = live_workers().await;
    let before = current_document(&workers.owner);
    let private_before = workers
        .owner
        .store()
        .unwrap()
        .state
        .private_agent_ledger
        .clone();
    let (broker, job) = start_table(&mut workers, &mut documents, &mut network).await;
    let invocation = broker.status(job.id).unwrap().invocation();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut commits = 0;
    loop {
        workers.pump().unwrap();
        while let Ok(pending) = documents.try_recv() {
            assert!(!matches!(
                broker.status(job.id),
                Some(qos_actions::InvocationUpdate::Succeeded { .. })
            ));
            assert_eq!(
                current_document(&workers.owner),
                before,
                "mutation escaped the Store callback"
            );
            let owner = workers.owner.clone();
            let generation = owner.store().unwrap().state.generation;
            let (complete, outcome) = crate::agent_documents::apply_worker_document(
                &owner,
                pending,
                std::time::Instant::now() + std::time::Duration::from_secs(5),
            );
            outcome.unwrap();
            commits += 1;
            assert_eq!(owner.store().unwrap().state.generation, generation + 1);
            assert_ne!(current_document(&owner), before);
            assert_eq!(workers.documents.pending_exact_receipts().unwrap(), 1);
            assert!(!matches!(
                broker.status(job.id),
                Some(qos_actions::InvocationUpdate::Succeeded { .. })
            ));
            workers.committed().unwrap();
            complete();
        }
        while network.try_recv().is_ok() {}
        if let Some(status) = broker.status(job.id) {
            assert_eq!(status.invocation(), invocation);
            if status.is_terminal() {
                assert!(
                    matches!(status, qos_actions::InvocationUpdate::Succeeded { .. }),
                    "{status:?}"
                );
                break;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "actual worker action never completed"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(commits, 1);
    assert_eq!(workers.documents.pending_exact_receipts().unwrap(), 0);
    assert_eq!(workers.documents.outstanding_handoffs(), 0);
    let durable = current_document(&workers.owner);
    assert_eq!(
        workers.owner.store().unwrap().state.private_agent_ledger,
        private_before
    );
    workers.close().unwrap();
    drop(workers);
    let reopened = crate::Store::open(directory.path(), "agent-admission").unwrap();
    assert_eq!(
        reopened.peer().unwrap().document(WRITER, RECORD).unwrap(),
        durable
    );
}

#[tokio::test(flavor = "current_thread")]
async fn cloud_worker_merges_committed_bytes_after_outbox_drain_without_acknowledging_local_heads()
{
    let (_directory, mut workers, _documents, _network) = live_workers().await;
    let record = RecordId::for_tab(1);
    let records = workers.owners.open(WRITER, &workers.room).unwrap();
    records
        .with(&record, |document| splice(document, "unfinished local "))
        .unwrap();
    let local_heads = records
        .read(&record, |document| document.clone().get_heads())
        .unwrap();
    let remote_heads = {
        let mut store = workers.owner.store().unwrap();
        let mut sender = store.peer().unwrap();
        let device = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
        let mut remote =
            ui::automerge::AutoCommit::load(&sender.document(WRITER, RECORD).unwrap()).unwrap();
        let before = remote.get_heads();
        remote.set_actor(ui::automerge::ActorId::from(
            device.verifying_key().to_bytes().to_vec(),
        ));
        splice(&mut remote, "committed remote ");
        let now = qos_relay::clock::now_ns();
        let message = sender
            .publish_incremental(
                WRITER,
                RECORD,
                remote.save_after(&before),
                &device,
                crate::now_time(now),
            )
            .unwrap();
        store
            .receive(
                qos_peer_state::TransportOrigin::RelayDelta(device.verifying_key().to_bytes()),
                message,
                now,
            )
            .unwrap();
        let heads = store
            .peer()
            .unwrap()
            .document_heads(WRITER, RECORD)
            .unwrap();
        let mut state = store.state.clone();
        state.outbox.clear();
        store.commit(state).unwrap();
        heads
    };
    workers.committed().unwrap();
    let merged = records
        .read(&record, |document| document.clone().save())
        .unwrap();
    let mut merged_doc = ui::automerge::AutoCommit::load(&merged).unwrap();
    for head in local_heads.iter().chain(remote_heads.iter()) {
        assert!(
            merged_doc.get_change_by_hash(head).is_some(),
            "committed merge discarded owned history"
        );
    }
    assert!(
        workers
            .cache
            .pending_writes()
            .iter()
            .any(|write| write.key == storage_key(WRITER, &workers.room, RECORD))
    );
    let actual = current_document(&workers.owner);
    let mut actual = ui::automerge::AutoCommit::load(&actual).unwrap();
    assert!(
        local_heads
            .iter()
            .any(|head| actual.get_change_by_hash(head).is_none())
    );
    {
        let mut store = workers.owner.store().unwrap();
        let route = RouteSpec::record(WRITER, workers.room.parse().unwrap(), RECORD);
        store
            .commit_agent_document(
                &route,
                &workers.certificate,
                DocumentMutation::Snapshot(merged),
            )
            .unwrap();
    }
    workers.committed().unwrap();
    assert!(
        !workers
            .cache
            .pending_writes()
            .iter()
            .any(|write| write.key == storage_key(WRITER, &workers.room, RECORD))
    );
    workers.close().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn cloud_worker_withdraws_transport_grants_and_preserves_dirty_close_custody() {
    let (directory, mut workers, _documents, _network) = live_workers().await;
    let live = workers.publications().unwrap();
    assert!(live.ready && !live.grants.is_empty() && live.native_v5_ready);
    for grant in &live.grants {
        assert!(
            workers
                .broker
                .published_action_surfaces()
                .iter()
                .any(|surface| {
                    surface.live
                        && surface.app_id == grant.app_id
                        && (surface.route.record_id == grant.record_id
                            || (surface.route.record_id.is_empty()
                                && grant.record_id == RecordId::package_primary().as_str()))
                        && surface.actions.iter().any(|action| {
                            action.resource == grant.resource
                                && action.action == grant.action
                                && action.operation_kind == qos_actions::OperationKind::Document
                        })
                })
        );
    }
    workers.set_transport_ready(false);
    let withdrawn = workers.publications().unwrap();
    assert!(!withdrawn.ready && withdrawn.grants.is_empty());
    let original = std::fs::read(directory.path().join("state.json")).unwrap();
    let records = workers.owners.open(WRITER, &workers.room).unwrap();
    let record = RecordId::for_tab(1);
    records
        .with(&record, |document| {
            splice(document, "original dirty owner ")
        })
        .unwrap();
    let dirty = records
        .read(&record, |document| document.clone().save())
        .unwrap();
    let error = workers.close().unwrap_err();
    assert!(error.contains("custody preserved"), "{error}");
    let recovery: serde_json::Value = serde_json::from_slice(
        &std::fs::read(directory.path().join("actions/recovery.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(recovery["outcome"], "unknown");
    assert!(recovery["pending"].as_array().unwrap().iter().any(|write| {
        write["key"]["app"] == WRITER
            && write["key"]["record"] == RECORD
            && write["bytes"] == serde_json::json!(dirty)
            && write["actor"] == serde_json::json!(workers.certificate.device_public_key)
    }));
    assert_eq!(
        std::fs::read(directory.path().join("state.json")).unwrap(),
        original
    );
    let owner = workers.owner.clone();
    drop(workers);
    let (outbound, _network) = mpsc::channel(32);
    let error = AgentWorkers::start(directory.path(), owner, outbound)
        .err()
        .unwrap();
    assert!(error.contains("unresolved application recovery"), "{error}");
}

#[test]
fn cloud_worker_outbound_queue_full_or_closed_retires_signer() {
    for closed in [false, true] {
        let (_directory, owner) = selected();
        let (documents, _pending) = HostDocuments::new(Arc::new(owner.ordinary()));
        let (sender, receiver) = mpsc::channel(1);
        let sink = NetworkSink {
            sender,
            documents: documents.clone(),
            owner,
            peer: 42,
        };
        if closed {
            drop(receiver);
        }
        let artifact = collab_types::ArtifactId(uuid::Uuid::new_v4());
        sink.scrollback_request(artifact, 2, 1);
        if !closed {
            assert!(documents.health().is_ok());
            sink.scrollback_request(artifact, 1, 1);
        }
        assert!(
            documents
                .health()
                .unwrap_err()
                .contains("outbound queue unavailable")
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn cloud_worker_postcommit_lost_reply_never_reports_success_or_reexecutes() {
    let (directory, mut workers, mut documents, mut network) = live_workers().await;
    let before = current_document(&workers.owner);
    let (broker, job) = start_table(&mut workers, &mut documents, &mut network).await;
    let invocation = broker.status(job.id).unwrap().invocation();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        workers.pump().unwrap();
        if let Ok(pending) = documents.try_recv() {
            let owner = workers.owner.clone();
            let (complete, outcome) = crate::agent_documents::apply_worker_document(
                &owner,
                pending,
                std::time::Instant::now() + std::time::Duration::from_secs(5),
            );
            outcome.unwrap();
            workers.committed().unwrap();
            workers.fail("postcommit lifecycle lost reply; outcome unknown");
            drop(complete);
            break;
        }
        while network.try_recv().is_ok() {}
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let committed = current_document(&workers.owner);
    assert_ne!(committed, before);
    let status = broker.status(job.id).unwrap();
    assert_eq!(status.invocation(), invocation);
    assert!(!matches!(
        status,
        qos_actions::InvocationUpdate::Succeeded { .. }
    ));
    assert_eq!(workers.documents.pending_exact_receipts().unwrap(), 1);
    assert!(workers.pump().is_err());
    assert!(workers.publications().is_err());
    workers.close().unwrap();
    drop(broker);
    drop(workers);
    let owner = Arc::new(RuntimeOwner::open(directory.path(), "agent-admission").unwrap());
    assert_eq!(current_document(&owner), committed);
    let (sender, mut network) = mpsc::channel(32);
    let (mut reopened, mut documents) =
        AgentWorkers::start(directory.path(), owner, sender).unwrap();
    // Fresh Chat workers may request a snapshot of their already committed
    // record. That is not invocation replay: inspect every actual request and
    // permit only mutations whose heads are already in the durable Store.
    for _ in 0..20 {
        reopened.pump().unwrap();
        while let Ok(pending) = documents.try_recv() {
            let owner = reopened.owner.clone();
            let (complete, outcome) = pending.apply_scoped_fenced(
                std::time::Instant::now() + std::time::Duration::from_secs(5),
                |route, certificate, mutation, authorization| {
                    let mut store = owner.store()?;
                    let bytes = store
                        .peer()
                        .map_err(|e| e.to_string())?
                        .document(&route.app_id, route.record.as_deref().unwrap_or(""))
                        .unwrap();
                    let mut durable = ui::automerge::AutoCommit::load(&bytes).unwrap();
                    let before = durable.get_heads();
                    match &mutation {
                        DocumentMutation::Snapshot(bytes) => {
                            durable
                                .merge(&mut ui::automerge::AutoCommit::load(bytes).unwrap())
                                .unwrap();
                        }
                        DocumentMutation::Incremental(bytes) => {
                            durable.load_incremental(bytes).unwrap();
                        }
                    }
                    assert_eq!(
                        before,
                        durable.get_heads(),
                        "reopen attempted an uncommitted document edit"
                    );
                    // Reopen snapshots carry no contributor; the final scoped
                    // evaluator must accept only already committed changes.
                    let authorization = authorization.expect("required worker custody");
                    store.commit_scoped_agent_document(route, certificate, mutation, authorization)
                },
            );
            outcome.unwrap();
            reopened.committed().unwrap();
            complete();
        }
        while network.try_recv().is_ok() {}
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(current_document(&reopened.owner), committed);
    reopened.close().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn cloud_worker_setup_failure_retains_original_record_and_captured_view() {
    let (directory, mut workers, _documents, _network) = live_workers().await;
    let records = workers.owners.open(WRITER, &workers.room).unwrap();
    let record = RecordId::for_tab(1);
    records
        .with(&record, |document| splice(document, "setup custody "))
        .unwrap();
    let dirty = records
        .read(&record, |document| document.clone().save())
        .unwrap();
    let worker = workers
        .workers
        .iter_mut()
        .find(|worker| worker.app == WRITER)
        .unwrap();
    let mut report = worker
        .runner
        .close()
        .into_iter()
        .find(|report| report.recovery.is_some())
        .unwrap();
    let captured = serde_json::to_value(&report.recovery).unwrap();
    report.view_error = Some("setup view capture failed".into());
    report.view_retry = None;
    workers.setup_error(ui::launch::LaunchError::ComponentSetup {
        source: Box::new(ui::launch::LaunchError::Operation("mount failed".into())),
        record: RecordCheckout::new(record, word_processor::definition().record_kind.clone()),
        records,
        reports: vec![report],
    });
    assert!(!workers.recovery.is_empty());
    assert!(workers.close().unwrap_err().contains("custody preserved"));
    let recovery: serde_json::Value = serde_json::from_slice(
        &std::fs::read(directory.path().join("actions/recovery.json")).unwrap(),
    )
    .unwrap();
    assert!(
        recovery["pending"]
            .as_array()
            .unwrap()
            .iter()
            .any(|write| write["bytes"] == serde_json::json!(dirty))
    );
    assert!(
        recovery["reports"]
            .as_array()
            .unwrap()
            .iter()
            .any(|report| report["view"] == captured
                && report["view_error"] == "setup view capture failed")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn cloud_worker_committed_host_revocation_withdraws_publications_and_retains_custody() {
    let (directory, mut workers, _documents, _network) = live_workers().await;
    use qos_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};
    let root = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
    let mut entry = RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: workers.certificate.device_public_key,
        revoked_at_ns: qos_relay::clock::now_ns(),
        reason: RevocationReason::Lost,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, &root).unwrap();
    {
        let mut store = workers.owner.store().unwrap();
        let certificate = store
            .agent_caller(device.verifying_key().to_bytes())
            .unwrap()
            .certificate()
            .clone();
        store
            .receive(
                qos_peer_state::TransportOrigin::RelayDelta(device.verifying_key().to_bytes()),
                NeighbourMsg::IdentityHello {
                    cert: Box::new(certificate),
                    revocations: vec![entry],
                },
                qos_relay::clock::now_ns(),
            )
            .unwrap();
    }
    assert!(workers.publications().is_err());
    assert!(workers.pump().is_err());
    assert!(workers.close().unwrap_err().contains("custody preserved"));
    assert!(directory.path().join("actions/recovery.json").is_file());
}

#[tokio::test(flavor = "current_thread")]
async fn cloud_worker_start_never_implicitly_selects_document_or_private_writer() {
    let (directory, store, _) = crate::agent_owner_tests::enrolled();
    let before = std::fs::read(directory.path().join("state.json")).unwrap();
    drop(store);
    let owner = Arc::new(RuntimeOwner::open(directory.path(), "agent-admission").unwrap());
    let (sender, _network) = mpsc::channel(32);
    let error = AgentWorkers::start(directory.path(), owner, sender)
        .err()
        .unwrap();
    assert!(error.contains("explicitly selected V5"), "{error}");
    assert_eq!(
        std::fs::read(directory.path().join("state.json")).unwrap(),
        before
    );
    assert!(!directory.path().join("actions/journal").exists());
}

#[tokio::test(flavor = "current_thread")]
async fn cloud_worker_publications_exclude_real_ordinary_surface_on_same_route() {
    let (_directory, mut workers, mut documents, mut network) = live_workers().await;
    let key = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
    let certificate = workers
        .owner
        .store()
        .unwrap()
        .agent_caller(key.verifying_key().to_bytes())
        .unwrap()
        .certificate()
        .clone();
    let presentation = runtime_collab::CertifiedJoinIdentity {
        workspace_id: workers.room.clone(),
        device_certificate: serde_json::to_vec(&certificate).unwrap(),
        proof: key
            .sign(&runtime_identity::collaboration_join_signing_bytes(
                &workers.room,
            ))
            .to_bytes()
            .to_vec(),
    }
    .encode()
    .unwrap();
    // This is the actual enrolled person device, on its ordinary permitted
    // channel. Its surface must not impersonate the host-owned worker.
    let route = CrdtRoute::record(WRITER, &workers.room, RECORD);
    let stream = workers
        .broker
        .connect_local_worker(route.clone(), &certificate)
        .unwrap();
    let mut ordinary = CollaborationManager::for_route_fd_with_identity(
        stream.into_raw_fd(),
        WRITER,
        workers.room.parse().unwrap(),
        Some(RECORD.into()),
        None,
        CertifiedEndpointIdentity::new(presentation, key),
    )
    .unwrap();
    assert!(
        ordinary.send(ui::collaboration::CollabRequest::PublishActions {
            app_id: WRITER.into(),
            actions: vec![ui::collaboration::ExportedAction {
                resource: "ordinary-surface".into(),
                action: "must-not-be-granted".into(),
                resource_kind: "document".into(),
                resource_label: "Ordinary peer".into(),
                action_label: "Ordinary edit".into(),
                description: None,
                operation_kind: "document".into(),
                params_json: "[]".into(),
                contract_json: serde_json::to_string(&qos_actions::ActionContract::default())
                    .unwrap(),
                input: ui::collaboration::ExportedInput::None,
            }],
        })
    );
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let ordinary_peer = loop {
        workers.pump().unwrap();
        while let Ok(pending) = documents.try_recv() {
            apply_document(&mut workers, pending);
        }
        while network.try_recv().is_ok() {}
        if let Some(surface) =
            workers
                .broker
                .published_action_surfaces()
                .into_iter()
                .find(|surface| {
                    surface.live
                        && surface.route == route
                        && surface
                            .actions
                            .iter()
                            .any(|action| action.action == "must-not-be-granted")
                })
        {
            break surface.peer_id;
        }
        assert!(ordinary.is_connected());
        assert!(
            tokio::time::Instant::now() < deadline,
            "ordinary SDK did not publish real surface"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    };
    assert!(
        !workers
            .broker
            .is_current_host_worker(ordinary_peer, &route, &workers.certificate)
            .unwrap()
    );
    let publication = workers.publications().unwrap();
    assert!(publication.ready);
    assert!(
        !publication
            .grants
            .iter()
            .any(|grant| grant.action == "must-not-be-granted")
    );
    drop(ordinary);
    workers.close().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn cloud_worker_real_primary_chat_grants_validate_as_offer_and_retained_intent() {
    use qos_agent_control_protocol::{
        AgentBinding, AgentCapabilities, AgentRequest, AgentResponse, AgentResponseKind,
        AgentUnavailableReason, ExpectedResponse, GrantOffer,
    };
    use qos_peer_state::owner_commit::{
        AgentIntentV2, AgentInvocationScopeV2, AgentProviderRouteV2,
    };
    let (_directory, mut workers, mut documents, mut network) = live_workers().await;
    let publications = workers.publications().unwrap();
    assert!(
        workers
            .broker
            .published_action_surfaces()
            .iter()
            .any(|surface| {
                surface.live
                    && surface.app_id == "so.tilo.chat"
                    && surface.route.record_id.is_empty()
                    && surface
                        .actions
                        .iter()
                        .any(|action| action.operation_kind == qos_actions::OperationKind::Document)
            }),
        "the regression must include the actual primary Chat worker"
    );
    assert!(
        publications
            .grants
            .iter()
            .any(|grant| grant.app_id == "so.tilo.chat")
    );
    let chat_grant = publications
        .grants
        .iter()
        .find(|grant| grant.app_id == "so.tilo.chat" && grant.action == "collapse")
        .cloned()
        .expect("real seeded Chat category action");
    let invocation = uuid::Uuid::new_v4().to_string();
    let mut offer: Option<GrantOffer> = None;
    crate::agent_runtime::publish_runtime_state(
        &workers.owner,
        Some(&workers),
        &mut offer,
        false,
        false,
    )
    .unwrap();
    let offer = offer.expect("actual lifecycle offer from mounted workers");
    assert_eq!(offer.actions, publications.grants);
    let offer_id = offer.id.clone();
    let descriptor_revision = offer.revision.clone();
    {
        let store = workers.owner.store().unwrap();
        let caller = store
            .agent_caller(
                ed25519_dalek::SigningKey::from_bytes(&[8; 32])
                    .verifying_key()
                    .to_bytes(),
            )
            .unwrap();
        let binding = AgentBinding {
            machine: store.state.machine_id.clone(),
            room: workers.room.clone(),
            host: workers.room.clone(),
            device: hex::encode(caller.certificate().device_public_key),
        };
        // This is validation of inert published data. Provider/process witnesses
        // remain absent, and the test makes no runtime-readiness claim.
        let response = AgentResponse::Capabilities {
            version: 1,
            invocation_id: invocation.clone(),
            binding: binding.clone(),
            capabilities: AgentCapabilities {
                list: true,
                spawn: false,
                message: false,
                grant_offers: vec![offer],
                unavailable_reasons: vec![AgentUnavailableReason::NotConfigured],
            },
        };
        qos_agent_control_protocol::validate_response(
            &response,
            &ExpectedResponse {
                invocation_id: invocation,
                binding,
                kind: Some(AgentResponseKind::Capabilities),
            },
        )
        .expect("real published offer must satisfy the canonical agent protocol");
        let request = AgentRequest::Spawn {
            version: 1,
            name: "Actual document grant".into(),
            task: "Use the current workspace documents".into(),
            grant_set_id: offer_id,
        };
        let intent = AgentIntentV2 {
            scope: AgentInvocationScopeV2 {
                machine: store.state.machine_id.clone(),
                room: workers.room.clone(),
                host: workers.room.clone(),
                caller: caller.certificate().clone(),
            },
            route: AgentProviderRouteV2 {
                app_id: qos_action_broker::agent_control::APP_ID.into(),
                app_peer: qos_relay::room_workspace_id(&workers.room)
                    .unwrap()
                    .to_string(),
                cluster_id: workers.room.clone(),
                record_id: String::new(),
                resource: format!("workspace:{}:agents", workers.room),
                resource_kind: "workspace.agents".into(),
                action: "spawn".into(),
            },
            descriptor_revision,
            request: String::from_utf8(
                qos_agent_control_protocol::canonical_request(&request).unwrap(),
            )
            .unwrap(),
            grants: publications.grants,
        };
        assert_eq!(intent.validate(&workers.room).unwrap(), request);
        assert!(
            intent
                .grants
                .iter()
                .filter(|grant| grant.app_id == "so.tilo.chat")
                .all(|grant| grant.record_id == "@qos:package-primary")
        );
    }
    let surface = workers
        .broker
        .published_action_surfaces()
        .into_iter()
        .find(|surface| {
            surface.live
                && surface.app_id == chat_grant.app_id
                && surface.route.record_id.is_empty()
                && surface.actions.iter().any(|action| {
                    action.resource == chat_grant.resource && action.action == chat_grant.action
                })
        })
        .expect("canonical grant must still name the actual raw-primary route");
    let before = workers
        .owner
        .store()
        .unwrap()
        .peer()
        .unwrap()
        .document("so.tilo.chat", "")
        .unwrap();
    let (broker, job) = certified_start(
        &mut workers,
        &mut documents,
        &mut network,
        &surface,
        &chat_grant.resource,
        &chat_grant.action,
    )
    .await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        workers.pump().unwrap();
        while let Ok(pending) = documents.try_recv() {
            apply_document(&mut workers, pending);
        }
        while network.try_recv().is_ok() {}
        if let Some(status) = broker.status(job.id) {
            if status.is_terminal() {
                assert!(
                    matches!(status, qos_actions::InvocationUpdate::Succeeded { .. }),
                    "{status:?}"
                );
                break;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "real primary Chat action did not complete"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_ne!(
        workers
            .owner
            .store()
            .unwrap()
            .peer()
            .unwrap()
            .document("so.tilo.chat", "")
            .unwrap(),
        before
    );
    workers.close().unwrap();
}

#[test]
fn cloud_policy_clock_cached_worker_permission_dies_with_source() {
    use collab_types::caps::{CapDecision, ResourceRef, Verb};
    let (_dir, owner) = selected();
    let clock = owner.policy_context().clock();
    let source = clock.new_origin();
    owner
        .policy_context()
        .bind_source(source.capture().unwrap())
        .unwrap();
    let store = owner.store().unwrap();
    let peer = store.peer().unwrap();
    let context = permission(&owner, &peer, peer.authority()).unwrap();
    drop(store);
    let resource = ResourceRef::App(WRITER.into());
    assert_eq!(context.decide(&resource, Verb::Write), CapDecision::Allow);
    {
        let mut store = owner.store().unwrap();
        let request = crate::ActionRequest {
            version: 1,
            room: store.state.room.clone(),
            invocation_id: "clock-policy-unchanged-progress".into(),
            operation: crate::NativeOperation::WriterInsert {
                text: "normal progress".into(),
            },
        };
        assert_eq!(
            store
                .action(peer.authority().device_public_key, &request, 0)
                .unwrap()
                .status,
            "completed"
        );
    }
    assert_eq!(
        context.decide(&resource, Verb::Write),
        CapDecision::Allow,
        "ordinary document progress must not invalidate unchanged policy"
    );
    source.invalidate();
    assert_ne!(
        context.decide(&resource, Verb::Write),
        CapDecision::Allow,
        "cached worker policy allowed an effect after source retirement"
    );
}

#[test]
fn cloud_policy_clock_worker_sampler_checks_current_governance_before_next_pump() {
    use automerge::{ROOT, ReadDoc, ScalarValue, transaction::Transactable};
    use collab_types::caps::{CapDecision, CapOverride, ResourceRef, Verb};
    let (_dir, owner) = selected();
    let context = {
        let store = owner.store().unwrap();
        let peer = store.peer().unwrap();
        permission(&owner, &peer, peer.authority()).unwrap()
    };
    let resource = ResourceRef::App(WRITER.into());
    assert_eq!(context.decide(&resource, Verb::Write), CapDecision::Allow);
    {
        let mut store = owner.store().unwrap();
        let mut doc = store.peer().unwrap().admitted_room().unwrap().clone();
        doc.set_actor(automerge::ActorId::from(
            store.key().verifying_key().to_bytes().to_vec(),
        ));
        let caps = doc.get(ROOT, "capabilities").unwrap().unwrap().1;
        let cap = CapOverride::restrict_role_in_app(collab_types::Role::Owner, WRITER, Verb::Read);
        doc.put(
            &caps,
            "clock-worker-restrict",
            ScalarValue::Bytes(serde_json::to_vec(&cap).unwrap()),
        )
        .unwrap();
        doc.commit();
        let message = qos_peer_state::NeighbourMsg::SyncResp {
            room: store.state.room.clone(),
            changes: doc
                .get_changes(&[])
                .into_iter()
                .map(|c| c.raw_bytes().to_vec())
                .collect(),
        };
        let now = store.policy_now_ns().unwrap();
        store
            .receive(
                qos_peer_state::TransportOrigin::RelayDelta(store.key().verifying_key().to_bytes()),
                message,
                now,
            )
            .unwrap();
    }
    assert_ne!(
        context.decide(&resource, Verb::Write),
        CapDecision::Allow,
        "cached capabilities escaped fresh actual Store governance validation"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn cloud_workers_source_root_alias_uses_held_directory_identity() {
    use crate::https_owner::{CloudStoreSource, Current};
    use qos_action_broker::peer_actions::OwnerPolicyClock;
    let (directory, mut store, _) = crate::agent_owner_tests::enrolled();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    drop(store);
    // Keep an actual noncanonical spelling in the selected Store/Source. The
    // original integration RED also resolves /var to /private/var on macOS.
    let alias = directory.path().join(".");
    let store = crate::Store::open(&alias, "agent-admission").unwrap();
    assert_ne!(
        store.root.as_os_str(),
        alias.canonicalize().unwrap().as_os_str()
    );
    let clock = OwnerPolicyClock::new().unwrap();
    let source = CloudStoreSource::bind_with_clock(&store, clock.clone()).unwrap();
    drop(store);
    let Current::Current(time) = source.with_current(|turn| turn.policy_time().unwrap()) else {
        panic!("actual selected Source unavailable");
    };
    let owner = Arc::new(
        RuntimeOwner::open_with_source(
            &alias,
            "agent-admission",
            clock,
            time.clone(),
            source.clone(),
        )
        .unwrap(),
    );
    let unrelated = tempfile::tempdir().unwrap();
    let (outbound, _network) = mpsc::channel(32);
    let error = match AgentWorkers::start(unrelated.path(), owner.clone(), outbound.clone()) {
        Ok((mut workers, _)) => {
            let _ = workers.close();
            panic!("different directory was accepted as worker owner");
        }
        Err(error) => error,
    };
    assert_eq!(error, "worker root differs from Store owner");
    assert!(!unrelated.path().join("actions").exists());
    assert!(
        time.now_ns().is_ok(),
        "unrelated request retired valid Source"
    );
    let (mut workers, _documents) = AgentWorkers::start(&alias, owner, outbound)
        .expect("same held directory alias must start actual worker assembly");
    assert!(workers.app_broker().is_ok());
    workers.close().unwrap();
}

fn registered_source_owner() -> (
    tempfile::TempDir,
    Arc<RuntimeOwner>,
    crate::https_owner::CloudStoreSource,
) {
    use crate::https_owner::{CloudStoreSource, Current};
    use qos_action_broker::peer_actions::OwnerPolicyClock;
    let (directory, legacy) = selected();
    drop(legacy);
    let mut store = crate::Store::open(directory.path(), "agent-admission").unwrap();
    let clock = OwnerPolicyClock::new().unwrap();
    store
        .activate_delivery_storage(clock.now_ns().unwrap())
        .unwrap();
    let source = CloudStoreSource::bind_with_clock(&store, clock.clone()).unwrap();
    drop(store);
    let Current::Current(time) = source.with_current(|turn| turn.policy_time().unwrap()) else {
        panic!("actual selected Source unavailable");
    };
    let owner = Arc::new(
        RuntimeOwner::open_with_source(
            directory.path(),
            "agent-admission",
            clock,
            time,
            source.clone(),
        )
        .unwrap(),
    );
    (directory, owner, source)
}

fn settle_startup_document(workers: &mut AgentWorkers, pending: PendingHostDocument) {
    let owner = workers.owner.clone();
    let (complete, outcome) = pending.apply_scoped_fenced(
        std::time::Instant::now() + std::time::Duration::from_secs(5),
        |route, certificate, mutation, authorization| {
            let authorization = authorization.expect("required worker custody");
            let record = route.record.as_deref().unwrap_or("");
            let heads = |store: &crate::Store| {
                store
                    .peer()
                    .unwrap()
                    .document_heads(&route.app_id, record)
                    .unwrap()
            };
            let mut store = owner
                .store()
                .map_err(qos_action_broker::host_documents::HostDocumentTurnError::Refused)?;
            let before = heads(&store);
            let committed =
                store.commit_scoped_agent_document(route, certificate, mutation, authorization)?;
            assert_eq!(
                heads(&store),
                before,
                "startup publication changed the document"
            );
            Ok(committed)
        },
    );
    outcome.unwrap();
    workers.committed().unwrap();
    complete();
}

async fn registered_nonmutating_pump(
    workers: &mut AgentWorkers,
    network: &mut mpsc::Receiver<NeighbourMsg>,
    documents: &mut mpsc::Receiver<PendingHostDocument>,
) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        workers
            .pump()
            .expect("actual registered authorizer must precede the first pump");
        while network.try_recv().is_ok() {}
        // Startup publications pass the actual final scoped evaluator. They
        // carry no invocation contributor, so only already committed history
        // may be admitted: document heads must not change.
        while let Ok(pending) = documents.try_recv() {
            settle_startup_document(workers, pending);
        }
        let surfaces = workers.broker.published_action_surfaces();
        if !workers.workers.is_empty()
            && workers.workers.iter().all(|worker| {
                surfaces.iter().any(|surface| {
                    surface.live
                        && surface.app_id == worker.app
                        && surface.route.record_id == worker.record
                        && workers
                            .broker
                            .is_current_host_worker(
                                surface.peer_id,
                                &surface.route,
                                &workers.certificate,
                            )
                            .unwrap()
                })
            })
        {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            eprintln!(
                "registered Cloud timeout workers={} surfaces={} peers={} documents={}",
                workers.workers.len(),
                surfaces.len(),
                workers.broker.peer_count(),
                documents.len()
            );
            for worker in workers.workers.iter_mut().take(16) {
                eprintln!(
                    "mounted app={} connected={}",
                    worker.app.chars().take(128).collect::<String>(),
                    worker.runner.app_mut().collaboration_is_connected()
                );
                if surfaces.is_empty() {
                    let coverage = worker.runner.app_mut().control_action_coverage();
                    let stale = coverage
                        .iter()
                        .filter(|row| row.status == ui::app::ControlBindingStatus::StaleAddress)
                        .count();
                    eprintln!("control coverage total={} stale={stale}", coverage.len());
                    for row in coverage
                        .iter()
                        .filter(|row| row.status == ui::app::ControlBindingStatus::StaleAddress)
                        .take(16)
                    {
                        eprintln!(
                            "control id={} status={:?}",
                            row.binding.control_id.chars().take(128).collect::<String>(),
                            row.status
                        );
                    }
                }
            }
            for surface in surfaces.iter().take(16) {
                let current = match workers.broker.is_current_host_worker(
                    surface.peer_id,
                    &surface.route,
                    &workers.certificate,
                ) {
                    Ok(true) => "true",
                    Ok(false) => "false",
                    Err(_) => "error",
                };
                eprintln!(
                    "surface app={} live={} current={} mounted_route={} actions={}",
                    surface.app_id.chars().take(128).collect::<String>(),
                    surface.live,
                    current,
                    workers
                        .workers
                        .iter()
                        .any(|worker| worker.app == surface.app_id
                            && worker.record == surface.route.record_id),
                    surface.actions.len()
                );
            }
            let _ = workers.close();
            panic!("real registered Cloud worker surface missing");
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn cloud_worker_registered_source_pumps_and_refuses_absent_invocation() {
    let (directory, owner, _source) = registered_source_owner();
    let (outbound, mut network) = mpsc::channel(32);
    let (mut workers, mut documents) =
        AgentWorkers::start(directory.path(), owner.clone(), outbound).unwrap();
    workers.set_transport_ready(true);
    registered_nonmutating_pump(&mut workers, &mut network, &mut documents).await;
    let before = std::fs::read(directory.path().join("state.json")).unwrap();
    let generation = owner.store().unwrap().state.generation;
    let request = ui::host_worker::PublishedInvocationRequest {
        wire_id: u64::MAX,
        invocation: qos_actions::InvocationId(uuid::Uuid::new_v4()),
        address: qos_actions::ActionAddress::new("absent.resource", "absent-action"),
        argument: None,
        actor: workers.principal.clone(),
    };
    for worker in &workers.workers {
        let error = (worker.authorizer)(&request).unwrap_err();
        assert_eq!(error, "original invocation is not retained");
    }
    assert_eq!(owner.store().unwrap().state.generation, generation);
    assert_eq!(
        std::fs::read(directory.path().join("state.json")).unwrap(),
        before
    );
    // No queued document is acknowledged in this wiring-only regression.
    let closed = workers.close();
    if let Err(error) = closed {
        assert!(
            error.contains("custody preserved in application recovery"),
            "{error}"
        );
        assert!(directory.path().join("actions/recovery.json").is_file());
    }
    assert_eq!(
        std::fs::read(directory.path().join("state.json")).unwrap(),
        before
    );
}

#[tokio::test(flavor = "current_thread")]
async fn cloud_worker_registered_source_retirement_refuses_pump_and_connection() {
    let (directory, owner, source) = registered_source_owner();
    let (outbound, mut network) = mpsc::channel(32);
    let (mut workers, mut documents) =
        AgentWorkers::start(directory.path(), owner, outbound).unwrap();
    workers.set_transport_ready(true);
    registered_nonmutating_pump(&mut workers, &mut network, &mut documents).await;
    let before = std::fs::read(directory.path().join("state.json")).unwrap();
    source.retire();
    assert!(workers.pump().is_err());
    let worker = &workers.workers[0];
    assert!(
        workers
            .broker
            .connect_local_worker_with_authorizer(
                CrdtRoute::record(&worker.app, &workers.room, &worker.record),
                &workers.certificate,
            )
            .is_err()
    );
    assert_eq!(
        std::fs::read(directory.path().join("state.json")).unwrap(),
        before
    );
    let error = workers.close().unwrap_err();
    assert!(
        error.contains("custody preserved in application recovery"),
        "{error}"
    );
    assert!(directory.path().join("actions/recovery.json").is_file());
    assert_eq!(
        std::fs::read(directory.path().join("state.json")).unwrap(),
        before
    );
}
