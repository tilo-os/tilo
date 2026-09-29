use super::*;
use tilo_actions::Action;

pub(crate) fn selected() -> (tempfile::TempDir, Arc<RuntimeOwner>) {
    let directory = selected_store();
    let owner = Arc::new(RuntimeOwner::open(directory.path(), "agent-admission").unwrap());
    (directory, owner)
}

/// [`selected`] with an owner policy clock the test advances itself, for
/// time budgets checked without waiting them out on the wall clock.
pub(crate) fn selected_with_clock() -> (
    tempfile::TempDir,
    Arc<RuntimeOwner>,
    tilo_action_broker::peer_actions::PolicyClockTestControl,
) {
    let directory = selected_store();
    let (clock, control) = tilo_action_broker::peer_actions::OwnerPolicyClock::for_test(
        tilo_delivery_protocol::clock::now_ns(),
    );
    let owner = Arc::new(
        RuntimeOwner::open_with_clock(directory.path(), "agent-admission", clock).unwrap(),
    );
    (directory, owner, control)
}

fn selected_store() -> tempfile::TempDir {
    // The actual broker and runtime threads open this Store too.
    crate::storage::test_journal::journal_this_process();
    // The workers install the managed sender under the Store root.
    let (directory, mut store, _) = crate::agent_owner_tests::enrolled_admitted();
    let mut peer = store.peer().unwrap();
    peer.select_admission_v5().unwrap();
    let mut state = store.state.clone();
    state.bound.as_mut().unwrap().peer = peer.durable().unwrap();
    store.commit(state).unwrap();
    drop(store);
    directory
}

/// The broker thread opens the owner's Store on collaboration joins. Here, as
/// in production, an unchanged Store is reused on any thread without file or
/// directory barriers, so a slow disk cannot stall joins behind them.
#[test]
fn broker_thread_store_opens_take_no_durability_barrier() {
    let (_directory, owner) = selected();
    drop(owner.store().unwrap());
    let reconciles = std::thread::spawn(move || {
        let before = crate::storage::reconciles_for_test();
        for _ in 0..3 {
            drop(owner.store().unwrap());
        }
        crate::storage::reconciles_for_test() - before
    })
    .join()
    .unwrap();
    assert_eq!(reconciles, 0, "a read-only open paid durability barriers");
}

/// On a broker-like thread, after this process forgot every durable file it
/// knew (a fresh process, or files changed outside it): the reconciliations
/// three request-handling Store opens pay, then the barriers one commit
/// through the same owner path pays.
fn broker_thread_barriers_after_cache_miss(owner: Arc<RuntimeOwner>) -> (u64, u64) {
    crate::storage::forget_durable_for_test();
    std::thread::spawn(move || {
        let before = crate::storage::reconciles_for_test();
        for _ in 0..3 {
            drop(owner.store().unwrap());
        }
        let reads = crate::storage::reconciles_for_test() - before;
        let mut store = owner.store().unwrap();
        let mut state = store.state.clone();
        state.generation += 1;
        let before = crate::storage::write_barriers_for_test();
        store.commit(state).unwrap();
        (reads, crate::storage::write_barriers_for_test() - before)
    })
    .join()
    .unwrap()
}

/// `serve` installs one long-lived Store per root (`owner_cell`) before the
/// broker starts, reconciling it once on its own thread. Broker request
/// handling borrows that Store, so even when nothing on disk is known durable
/// it waits on no file or directory barrier; only opens from disk would.
#[test]
fn broker_thread_takes_no_durability_barrier_on_a_cache_miss() {
    let (directory, owner) = selected();
    // The control: opened from disk, a cache miss pays the barriers again.
    let (reads, writes) = broker_thread_barriers_after_cache_miss(owner.clone());
    assert!(reads > 0);
    assert!(writes > 0);
    let _daemon = crate::owner_cell::install(directory.path(), "agent-admission").unwrap();
    let (reads, writes) = broker_thread_barriers_after_cache_miss(owner);
    assert_eq!(reads, 0, "broker request handling paid durability barriers");
    // No barrier was dropped: a commit through the long-lived Store still
    // returns only after its durability barrier.
    assert!(writes > 0, "a long-lived Store commit skipped its barrier");
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
    word_processor::actions::RichTextAction::SpliceText {
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

/// Owner-turn budget of the harness's own document drains. Production keeps
/// its fixed 5 s turn budget (a turn over it retires the signer, fail-closed,
/// and has its own tests). These tests drive that turn themselves on a
/// current-thread executor, often on a shared, fully loaded runner, where a
/// full Store commit alone can exceed 5 s; they check behaviour, not latency.
pub(crate) const HARNESS_TURN: std::time::Duration = std::time::Duration::from_secs(60);
/// How long a harness loop waits for workers, steps or replies to settle
/// before failing, sized for the same loaded runners.
pub(crate) const HARNESS_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

fn apply_document(workers: &mut AgentWorkers, pending: PendingHostDocument) {
    let owner = workers.owner.clone();
    let (complete, outcome) =
        crate::agent_documents::apply_worker_document(&owner, pending, HARNESS_TURN);
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
    let deadline = tokio::time::Instant::now() + HARNESS_WAIT;
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
    _pending: tilo_action_broker::peer_actions::PendingPeerAction,
}

async fn certified_start(
    workers: &mut AgentWorkers,
    documents: &mut mpsc::Receiver<PendingHostDocument>,
    network: &mut mpsc::Receiver<NeighbourMsg>,
    surface: &tilo_action_broker::collab_broker_capnp::PublishedSurface,
    resource: &str,
    action: &str,
) -> (AppActionBroker, CertifiedJob) {
    use tilo_delivery_protocol::peer_actions::ActionMessage;
    let invocation = tilo_actions::InvocationId::new();
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
            tokio::time::Instant::from_std(tilo_test_env::hang_deadline()),
            workers.owner.policy_time().unwrap(),
        )
        .unwrap();
    let broker = workers.app_broker().unwrap();
    let deadline = tokio::time::Instant::now() + HARNESS_WAIT;
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
                        tilo_delivery_protocol::peer_actions::ActionMessage::Observation { .. }
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
    let deadline = tokio::time::Instant::now() + HARNESS_WAIT;
    let mut commits = 0;
    loop {
        workers.pump().unwrap();
        while let Ok(pending) = documents.try_recv() {
            assert!(!matches!(
                broker.status(job.id),
                Some(tilo_actions::InvocationUpdate::Succeeded { .. })
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
                tilo_test_env::hang_bound(),
            );
            outcome.unwrap();
            commits += 1;
            assert_eq!(owner.store().unwrap().state.generation, generation + 1);
            assert_ne!(current_document(&owner), before);
            assert_eq!(workers.documents.pending_exact_receipts().unwrap(), 1);
            assert!(!matches!(
                broker.status(job.id),
                Some(tilo_actions::InvocationUpdate::Succeeded { .. })
            ));
            workers.committed().unwrap();
            complete();
        }
        while network.try_recv().is_ok() {}
        if let Some(status) = broker.status(job.id) {
            assert_eq!(status.invocation(), invocation);
            if status.is_terminal() {
                assert!(
                    matches!(status, tilo_actions::InvocationUpdate::Succeeded { .. }),
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
        let now = tilo_delivery_protocol::clock::now_ns();
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
                tilo_peer_state::TransportOrigin::RelayDelta(device.verifying_key().to_bytes()),
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
    // Scoped grants (default): every frozen scope names live worker records.
    for grant in &live.grants {
        let scope = tilo_agent_control_protocol::decode_scope_carrier(grant).unwrap();
        assert!(
            workers
                .broker
                .published_action_surfaces()
                .iter()
                .any(|surface| {
                    surface.live
                        && surface.app_id == scope.app_id
                        && tilo_agent_control_protocol::canonical_record(&surface.route.record_id)
                            .is_ok_and(|record| scope.covers_record(&record))
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
    let deadline = tokio::time::Instant::now() + HARNESS_WAIT;
    loop {
        workers.pump().unwrap();
        if let Ok(pending) = documents.try_recv() {
            let owner = workers.owner.clone();
            let (complete, outcome) = crate::agent_documents::apply_worker_document(
                &owner,
                pending,
                tilo_test_env::hang_bound(),
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
        tilo_actions::InvocationUpdate::Succeeded { .. }
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
                tilo_test_env::hang_bound(),
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
                            tilo_automerge::ApplyIncoming::merge_incoming(
                                &mut durable,
                                &mut ui::automerge::AutoCommit::load(bytes).unwrap(),
                            )
                            .unwrap();
                        }
                        DocumentMutation::Incremental(bytes) => {
                            tilo_automerge::ApplyIncoming::load_incoming(&mut durable, bytes)
                                .unwrap();
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
        record: Box::new(RecordCheckout::new(
            record,
            word_processor::definition().record_kind.clone(),
        )),
        recovery: ui::launch::LaunchRecovery::new(Some(records), vec![report]),
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
    use tilo_action_broker::peer_actions::CertificateAuthority;
    use tilo_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};
    // Broker verdicts are retained per Store observation; populate them twice
    // (the second from the retained verdict) before the revocation commit.
    let room: uuid::Uuid = workers.room.parse().unwrap();
    let route = tilo_action_broker::collab_broker::CrdtRoute::record(WRITER, &workers.room, RECORD);
    for _ in 0..2 {
        CertificateAuthority::authorize(&*workers.owner, room, &workers.certificate).unwrap();
        workers
            .owner
            .authorize_host_worker(&route, &workers.certificate)
            .unwrap();
    }
    let root = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
    let mut entry = RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: workers.certificate.device_public_key,
        revoked_at_ns: tilo_delivery_protocol::clock::now_ns(),
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
                tilo_peer_state::TransportOrigin::RelayDelta(device.verifying_key().to_bytes()),
                NeighbourMsg::IdentityHello {
                    cert: Box::new(certificate),
                    revocations: vec![entry],
                },
                tilo_delivery_protocol::clock::now_ns(),
            )
            .unwrap();
    }
    // The revocation commit is observed before any retained verdict is reused.
    assert!(CertificateAuthority::authorize(&*workers.owner, room, &workers.certificate).is_err());
    assert!(
        workers
            .owner
            .authorize_host_worker(&route, &workers.certificate)
            .is_err()
    );
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
                contract_json: serde_json::to_string(&tilo_actions::ActionContract::default())
                    .unwrap(),
                input: ui::collaboration::ExportedInput::None,
                view: None,
            }],
        })
    );
    let deadline = tokio::time::Instant::now() + HARNESS_WAIT;
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
    use tilo_agent_control_protocol::{
        AgentBinding, AgentCapabilities, AgentRequest, AgentResponse, AgentResponseKind,
        AgentUnavailableReason, ExpectedResponse, GrantOffer,
    };
    use tilo_peer_state::owner_commit::{AgentIntent, AgentInvocationScope, AgentProviderRoute};
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
                    && surface.actions.iter().any(|action| {
                        action.operation_kind == tilo_actions::OperationKind::Document
                    })
            }),
        "the regression must include the actual primary Chat worker"
    );
    assert!(
        publications
            .grants
            .iter()
            .any(|grant| grant.app_id == "so.tilo.chat")
    );
    // The frozen scopes cover the real seeded Chat category action.
    let collapse = workers
        .broker
        .published_action_surfaces()
        .into_iter()
        .filter(|surface| surface.live && surface.app_id == "so.tilo.chat")
        .find_map(|surface| {
            surface
                .actions
                .iter()
                .find(|action| action.action == "collapse")
                .cloned()
        })
        .expect("real seeded Chat category action");
    let chat_grant = tilo_agent_control_protocol::GrantAction {
        app_id: "so.tilo.chat".into(),
        record_id: "@tilo:package-primary".into(),
        resource: collapse.resource.clone(),
        action: collapse.action.clone(),
    };
    let scopes = tilo_agent_control_protocol::scopes_from_grants(&publications.grants).unwrap();
    assert!(scopes.iter().any(
        |scope| scope.covers(&tilo_agent_control_protocol::ScopeTarget {
            app_id: "so.tilo.chat",
            record_id: "@tilo:package-primary",
            resource: &collapse.resource,
            action: "collapse",
            class: Some(tilo_agent_control_protocol::OperationClass::Document),
            required: tilo_agent_control_protocol::GrantVerb::Write,
        })
    ));
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
        tilo_agent_control_protocol::validate_response(
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
        let intent = AgentIntent {
            scope: AgentInvocationScope {
                machine: store.state.machine_id.clone(),
                room: workers.room.clone(),
                host: workers.room.clone(),
                caller: caller.certificate().clone(),
            },
            route: AgentProviderRoute {
                app_id: tilo_action_broker::agent_control::APP_ID.into(),
                app_peer: tilo_relay::room_workspace_id(&workers.room)
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
                tilo_agent_control_protocol::canonical_request(&request).unwrap(),
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
                .all(|grant| grant.record_id == "@tilo:package-primary")
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
    let deadline = tokio::time::Instant::now() + HARNESS_WAIT;
    loop {
        workers.pump().unwrap();
        while let Ok(pending) = documents.try_recv() {
            apply_document(&mut workers, pending);
        }
        while network.try_recv().is_ok() {}
        if let Some(status) = broker.status(job.id)
            && status.is_terminal()
        {
            assert!(
                matches!(status, tilo_actions::InvocationUpdate::Succeeded { .. }),
                "{status:?}"
            );
            break;
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
        let message = tilo_peer_state::NeighbourMsg::SyncResp {
            room: store.state.room.clone(),
            changes: doc
                .get_changes(&[])
                .into_iter()
                .map(|c| c.raw_bytes().to_vec())
                .collect(),
        };
        let now = store.policy_now_ns().unwrap();
        let host_key = store.key().verifying_key().to_bytes();
        store
            .receive(
                tilo_peer_state::TransportOrigin::RelayDelta(host_key),
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
    use tilo_action_broker::peer_actions::OwnerPolicyClock;
    let (directory, mut store, _) = crate::agent_owner_tests::enrolled();
    store
        .activate_delivery_storage(tilo_delivery_protocol::clock::now_ns())
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
    let source = CloudStoreSource::bind_with_clock(store, clock.clone()).unwrap();
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
    use tilo_action_broker::peer_actions::OwnerPolicyClock;
    let (directory, legacy) = selected();
    drop(legacy);
    let mut store = crate::Store::open(directory.path(), "agent-admission").unwrap();
    let clock = OwnerPolicyClock::new().unwrap();
    store
        .activate_delivery_storage(clock.now_ns().unwrap())
        .unwrap();
    let source = CloudStoreSource::bind_with_clock(store, clock.clone()).unwrap();
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
        tilo_test_env::hang_bound(),
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
                .map_err(tilo_action_broker::host_documents::HostDocumentTurnError::Refused)?;
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
    let deadline = tokio::time::Instant::now() + HARNESS_WAIT;
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
        invocation: tilo_actions::InvocationId(uuid::Uuid::new_v4()),
        address: tilo_actions::ActionAddress::new("absent.resource", "absent-action"),
        argument: None,
        actor: workers.principal.clone(),
        participant: None,
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

/// Test-local mount settings for the next `AgentWorkers::start` on this
/// thread; restored on drop so other tests keep the unbounded default.
struct TestMountGuard;
impl TestMountGuard {
    fn set(budget: std::time::Duration, refused: &[(&str, &str)]) -> Self {
        Self::with_grace(budget, refused, UNMOUNT_GRACE)
    }
    fn with_grace(
        budget: std::time::Duration,
        refused: &[(&str, &str)],
        grace: std::time::Duration,
    ) -> Self {
        TEST_MOUNT.with(|test| {
            *test.borrow_mut() = (
                budget,
                refused
                    .iter()
                    .map(|(app, record)| ((*app).to_owned(), (*record).to_owned()))
                    .collect(),
                grace,
            )
        });
        Self
    }
}
impl Drop for TestMountGuard {
    fn drop(&mut self) {
        TEST_MOUNT.with(|test| {
            *test.borrow_mut() = (std::time::Duration::MAX, Default::default(), UNMOUNT_GRACE)
        });
    }
}
fn mounted(workers: &AgentWorkers) -> Vec<(String, String)> {
    workers
        .workers
        .iter()
        .map(|w| (w.app.clone(), w.record.clone()))
        .collect()
}

/// A refused worker connection (the broker's host-worker capacity, a failed
/// join) skips only that record: the mount succeeds without it, and the
/// record is retried with back-off and mounted once it is accepted.
#[tokio::test(flavor = "current_thread")]
async fn cloud_worker_refused_record_is_skipped_and_retried_not_fatal() {
    let (directory, owner) = selected();
    let guard = TestMountGuard::set(std::time::Duration::MAX, &[(WRITER, RECORD)]);
    let (outbound, _network) = mpsc::channel(32);
    let started = std::time::Instant::now();
    let (mut workers, _documents) = AgentWorkers::start(directory.path(), owner, outbound)
        .expect("one refused worker must not fail the whole mount");
    let key = (WRITER.to_owned(), RECORD.to_owned());
    assert!(!mounted(&workers).contains(&key));
    assert!(
        !workers.mounting(),
        "the refused record is skipped, not pending"
    );
    let (attempts, due) = workers.skipped[&key];
    assert_eq!(attempts, 1);
    // Measured from before the mount, so a slow host cannot make the back-off
    // look already spent.
    assert!(
        due >= started + MOUNT_RETRY_MIN,
        "retried later, with back-off"
    );
    // Not due yet: a pump does not retry it.
    workers.skipped.get_mut(&key).unwrap().1 = std::time::Instant::now() + MOUNT_RETRY_MAX;
    workers.pump().unwrap();
    assert!(!mounted(&workers).contains(&key));
    // Accepted again and due: the next pumps join and mount it.
    drop(guard);
    workers.skipped.get_mut(&key).unwrap().1 = std::time::Instant::now();
    tilo_test_env::wait_for("the retried worker to mount", || {
        workers.pump().unwrap();
        mounted(&workers).contains(&key).then_some(())
    });
    assert!(workers.skipped.is_empty());
    workers.close().unwrap();
}

/// A worker's join runs off the owner loop: while the broker has not
/// answered, every pump returns and the loop's other duties go on; the mount
/// completes on a later pump once the join answers.
#[tokio::test(flavor = "current_thread")]
async fn cloud_worker_join_never_blocks_the_owner_loop() {
    let (directory, owner) = selected();
    let guard = TestMountGuard::set(std::time::Duration::MAX, &[(WRITER, RECORD)]);
    let (outbound, _network) = mpsc::channel(32);
    let (mut workers, _documents) = AgentWorkers::start(directory.path(), owner, outbound)
        .expect("one refused worker must not fail the whole mount");
    let key = (WRITER.to_owned(), RECORD.to_owned());
    assert!(!mounted(&workers).contains(&key));
    drop(guard);
    // The next join is held until the test releases it, as a broker that
    // takes arbitrarily long to answer the primary route.
    let (release, gate) = std::sync::mpsc::channel();
    TEST_JOIN_GATE.with(|slot| *slot.borrow_mut() = Some(gate));
    workers.skipped.get_mut(&key).unwrap().1 = std::time::Instant::now();
    // Each of these pumps would hang if it waited for the held join.
    for _ in 0..20 {
        workers.pump().unwrap();
        workers.publications().unwrap();
    }
    assert!(workers.mounting(), "the held join is still under way");
    assert!(!mounted(&workers).contains(&key));
    release.send(()).unwrap();
    tilo_test_env::wait_for("the released join to mount its worker", || {
        workers.pump().unwrap();
        mounted(&workers).contains(&key).then_some(())
    });
    assert!(!workers.mounting());
    assert!(workers.skipped.is_empty());
    workers.close().unwrap();
}

/// Mounting is bounded per pass: a pass that already mounted one worker and
/// spent its budget leaves the rest pending for the next owner tick, and
/// publications are not ready until every validated record is mounted.
#[tokio::test(flavor = "current_thread")]
async fn cloud_worker_mounting_is_bounded_per_pass_and_ready_only_when_complete() {
    let (directory, owner) = selected();
    let rows = {
        let store = owner.store().unwrap();
        store
            .peer()
            .unwrap()
            .rows()
            .into_iter()
            .filter(|row| tilo_workspace_host::canonical_records::resolve(&row.app_id).is_some())
            .count()
    };
    assert!(rows >= 2, "the seed hosts at least two records ({rows})");
    let _guard = TestMountGuard::set(std::time::Duration::ZERO, &[]);
    let (outbound, _network) = mpsc::channel(32);
    let (mut workers, mut documents) =
        AgentWorkers::start(directory.path(), owner, outbound).unwrap();
    assert_eq!(
        workers.workers.len(),
        1,
        "one worker per pass at a zero budget"
    );
    assert!(workers.mounting());
    workers.set_transport_ready(true);
    assert!(!workers.publications().unwrap().ready);
    // Each pass mounts at most one more worker; the joins run off the
    // owner loop, so a pass never waits for one.
    tilo_test_env::wait_for("every validated record to mount", || {
        let before = workers.workers.len();
        workers.pump().unwrap();
        assert!(workers.workers.len() <= before + 1, "one worker per pass");
        (workers.workers.len() == rows && !workers.mounting()).then_some(())
    });
    assert!(!workers.mounting());
    let deadline = tokio::time::Instant::now() + HARNESS_WAIT;
    while !workers.publications().unwrap().ready {
        workers.pump().unwrap();
        while let Ok(pending) = documents.try_recv() {
            apply_document(&mut workers, pending);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "publication never became ready"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    workers.close().unwrap();
}

const NOTES: &str = "so.tilo.notes";

/// One desktop command of the enrolled person through the Cloud Store owner
/// (the same commit the broker's desktop action makes).
fn desktop(owner: &RuntimeOwner, command: tilo_desktop_model::commands::DesktopCommand) {
    use tilo_peer_state::desktop_command::{BoundCommand, CommandSource};
    use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
    let mut store = owner.store().unwrap();
    let peer = store.peer().unwrap();
    let caller = peer
        .certificates()
        .into_iter()
        .find(|cert| cert.device_public_key != store.key().verifying_key().to_bytes())
        .unwrap();
    let request = OwnerCommitRequest {
        room: store.state.room.clone(),
        operation: CommitOperation::DesktopCommand {
            invocation: uuid::Uuid::new_v4(),
            caller,
            command: BoundCommand {
                source: CommandSource {
                    room: store.state.room.clone(),
                    heads: peer
                        .admitted_room()
                        .unwrap()
                        .clone()
                        .get_heads()
                        .iter()
                        .map(|head| head.0)
                        .collect(),
                },
                command,
            },
        },
    };
    store.commit_desktop_command(&request, None).unwrap();
}
fn tab_locations(owner: &RuntimeOwner) -> Vec<tilo_desktop_model::commands::TabLocation> {
    let store = owner.store().unwrap();
    crate::agent_owner::workspace_tree(&store.peer().unwrap())
        .unwrap()
        .workspaces[0]
        .strips
        .iter()
        .flat_map(|strip| {
            strip.window_groups.iter().flat_map(move |group| {
                group
                    .tabs
                    .iter()
                    .map(move |tab| tilo_desktop_model::commands::TabLocation {
                        strip: strip.id.0,
                        group: group.id,
                        tab: tab.id,
                    })
            })
        })
        .collect()
}
fn notes_workers(workers: &AgentWorkers) -> Vec<String> {
    workers
        .workers
        .iter()
        .filter(|w| w.app == NOTES)
        .map(|w| w.record.clone())
        .collect()
}
/// What a connected relay does with the durable outbox: publish every queued
/// message and retire it. Without a relay the outbox, and with it every Store
/// commit, grows with each command a test makes.
pub(crate) fn relay_delivers_outbox(store: &mut crate::Store) {
    if store.state.version == crate::delivery::VERSION {
        loop {
            let batch = store.delivery_relay_batch().unwrap();
            let sent = batch.messages().count();
            if sent == 0 {
                break;
            }
            store.retire_delivery_relay_batch(&batch, sent).unwrap();
        }
    } else if !store.state.outbox.is_empty() {
        let mut state = store.state.clone();
        state.outbox.clear();
        store.commit(state).unwrap();
    }
}
/// Pump (settling worker publications) until `done` or a bounded deadline.
/// Between passes it waits for the next worker publication rather than
/// polling; the short timeout only lets grace periods elapse.
async fn pump_until(
    workers: &mut AgentWorkers,
    documents: &mut mpsc::Receiver<PendingHostDocument>,
    what: &str,
    done: impl Fn(&AgentWorkers) -> bool,
) {
    let deadline = tokio::time::Instant::from_std(tilo_test_env::hang_deadline());
    loop {
        workers.pump().unwrap();
        while let Ok(pending) = documents.try_recv() {
            apply_document(workers, pending);
        }
        if done(workers) {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        match tokio::time::timeout(std::time::Duration::from_millis(50), documents.recv()).await {
            Ok(Some(pending)) => apply_document(workers, pending),
            Ok(None) => panic!("worker documents closed: {what}"),
            Err(_) => {} // Only time-based work (an unmount grace) is due.
        }
    }
}

/// Open/close cycles of the worker-set test, and the cycles at each end whose
/// cost is compared. Every cycle leaves one more closed record; the records
/// each refresh validates are counted exactly, so a closed record that costs
/// work shows from the first cycle after it closed.
const CYCLES: usize = 12;
const WINDOW: usize = 4;

/// Workers follow open windows: repeated open/close cycles of Notes (each
/// opening a new record, as "Open Notes" does) never leave more workers than
/// open windows plus records inside their unmount grace, each refresh
/// validates only records with a window or a worker (never the closed ones),
/// and the worker set's own cost per open stays flat.
#[tokio::test(flavor = "current_thread")]
async fn cloud_worker_set_follows_open_windows_over_repeated_cycles() {
    let (directory, owner) = selected();
    let _guard =
        TestMountGuard::with_grace(std::time::Duration::MAX, &[], std::time::Duration::ZERO);
    let (outbound, mut network) = mpsc::channel(256);
    let (mut workers, mut documents) =
        AgentWorkers::start(directory.path(), owner.clone(), outbound).unwrap();
    workers.set_transport_ready(true);
    pump_until(&mut workers, &mut documents, "baseline", |w| !w.mounting()).await;
    let baseline = workers.workers.len();
    let windows = tab_locations(&owner).len();
    assert!(
        baseline <= windows,
        "no worker without a window ({baseline} > {windows})"
    );
    let mut latencies = vec![];
    let mut worker_times = vec![];
    let mut mount_times = vec![];
    let mut seen = std::collections::BTreeSet::new();
    for cycle in 0..CYCLES {
        let before = tab_locations(&owner);
        let began = std::time::Instant::now();
        desktop(
            &owner,
            tilo_desktop_model::commands::DesktopCommand::OpenApp { app: NOTES.into() },
        );
        let timing_before = workers.timing;
        pump_until(
            &mut workers,
            &mut documents,
            "open mounts its worker",
            |w| notes_workers(w).iter().any(|r| !seen.contains(r)),
        )
        .await;
        latencies.push(began.elapsed());
        worker_times.push(workers.timing.0 - timing_before.0);
        mount_times.push(workers.timing.1 - timing_before.1);
        let record = notes_workers(&workers)
            .into_iter()
            .find(|r| !seen.contains(r))
            .unwrap();
        seen.insert(record);
        assert!(
            workers.refreshed <= windows + 1,
            "cycle {cycle}: refresh validated {} records for {} windows",
            workers.refreshed,
            windows + 1
        );
        assert!(
            workers.workers.len() <= baseline + 1,
            "cycle {cycle}: {} workers for {} windows",
            workers.workers.len(),
            windows + 1
        );
        let opened = tab_locations(&owner)
            .into_iter()
            .find(|tab| !before.contains(tab))
            .unwrap();
        desktop(
            &owner,
            tilo_desktop_model::commands::DesktopCommand::CloseTab { location: opened },
        );
        pump_until(
            &mut workers,
            &mut documents,
            "close unmounts its worker",
            |w| w.workers.len() == baseline,
        )
        .await;
        while network.try_recv().is_ok() {}
        relay_delivers_outbox(&mut owner.store().unwrap());
    }
    assert_eq!(seen.len(), CYCLES, "every cycle opened a new record");
    let mean = |slice: &[std::time::Duration]| {
        slice.iter().sum::<std::time::Duration>() / slice.len() as u32
    };
    let (head, tail) = (..WINDOW, CYCLES - WINDOW..);
    let (first, last) = (mean(&latencies[head]), mean(&latencies[tail.clone()]));
    // Compare the cheapest cycle of each window: growth with closed records
    // raises every cycle's cost, while a loaded runner only adds spikes.
    let floor = |slice: &[std::time::Duration]| slice.iter().min().copied().unwrap_or_default();
    let (set_first, set_last) = (
        floor(&worker_times[head]),
        floor(&worker_times[tail.clone()]),
    );
    let (mount_first, mount_last) = (mean(&mount_times[head]), mean(&mount_times[tail]));
    let state = std::fs::metadata(directory.path().join("state.json")).map_or(0, |m| m.len());
    eprintln!(
        "open latency: first {WINDOW} mean {first:?}, last {WINDOW} mean {last:?} (state now {state} B); \
         worker-set bookkeeping {set_first:?} -> {set_last:?}; mount {mount_first:?} -> {mount_last:?}"
    );
    // The worker set's own cost per open must not grow with closed records.
    // (Mounting and end-to-end latency here also include Store opens and
    // commits, which scale with the Store size every created record adds in
    // this debug build: a Store concern, reported above, not gated here.)
    assert!(
        set_last <= set_first * 2 + std::time::Duration::from_millis(5),
        "worker-set cost grew with closed records: {set_first:?} -> {set_last:?}"
    );
    workers.close().unwrap();
}

/// A record without a window gets its worker while an active agent task is
/// granted it, and loses it (after the grace) once the task ends. A closed
/// record inside its grace period is not unmounted, and reopening cancels it.
#[tokio::test(flavor = "current_thread")]
async fn cloud_worker_agent_task_on_closed_record_gets_its_worker() {
    let (directory, owner) = selected();
    let _guard =
        TestMountGuard::with_grace(std::time::Duration::MAX, &[], std::time::Duration::ZERO);
    let (outbound, _network) = mpsc::channel(256);
    let (mut workers, mut documents) =
        AgentWorkers::start(directory.path(), owner.clone(), outbound).unwrap();
    workers.set_transport_ready(true);
    pump_until(&mut workers, &mut documents, "baseline", |w| !w.mounting()).await;
    let before = tab_locations(&owner);
    desktop(
        &owner,
        tilo_desktop_model::commands::DesktopCommand::OpenApp { app: NOTES.into() },
    );
    pump_until(&mut workers, &mut documents, "open", |w| {
        !notes_workers(w).is_empty()
    })
    .await;
    let record = notes_workers(&workers)[0].clone();
    let opened = tab_locations(&owner)
        .into_iter()
        .find(|tab| !before.contains(tab))
        .unwrap();
    desktop(
        &owner,
        tilo_desktop_model::commands::DesktopCommand::CloseTab { location: opened },
    );
    pump_until(&mut workers, &mut documents, "close", |w| {
        notes_workers(w).is_empty()
    })
    .await;
    // An agent task granted the closed record: its worker comes back.
    let task = ActiveAgentTask {
        task: uuid::Uuid::new_v4().to_string(),
        agent: uuid::Uuid::new_v4().to_string(),
        name: "Scribe".into(),
        grants: vec![GrantAction {
            app_id: NOTES.into(),
            record_id: record.clone(),
            resource: "note".into(),
            action: "append".into(),
        }],
    };
    let _ = workers.sync_agent_participants(std::slice::from_ref(&task));
    pump_until(&mut workers, &mut documents, "agent record mounts", |w| {
        notes_workers(w) == vec![record.clone()]
    })
    .await;
    // The task ends: released after the grace (zero here).
    let _ = workers.sync_agent_participants(&[]);
    pump_until(&mut workers, &mut documents, "task end releases", |w| {
        notes_workers(w).is_empty()
    })
    .await;
    // With a real grace, an unwanted worker stays mounted until it expires.
    workers.unmount_grace = std::time::Duration::from_secs(3600);
    let _ = workers.sync_agent_participants(std::slice::from_ref(&task));
    pump_until(&mut workers, &mut documents, "remount", |w| {
        notes_workers(w) == vec![record.clone()]
    })
    .await;
    let _ = workers.sync_agent_participants(&[]);
    for _ in 0..5 {
        workers.pump().unwrap();
    }
    assert_eq!(
        notes_workers(&workers),
        vec![record.clone()],
        "kept inside its grace"
    );
    assert!(
        workers
            .unwanted_since
            .contains_key(&(NOTES.to_owned(), record.clone()))
    );
    let _ = workers.sync_agent_participants(std::slice::from_ref(&task));
    workers.pump().unwrap();
    assert!(
        workers.unwanted_since.is_empty(),
        "wanted again cancels the grace"
    );
    workers.close().unwrap();
}
