//! macOS-runnable enrolled two-step handoff over the real components:
//! Store step ledger + RuntimeOwner (the durable parent owner), the real
//! Cloud broker and its enrolled recipient RPCs, the real certified workers
//! (Writer), the Cloud managed sender over its real private file, and an
//! in-process enrolled Agent client speaking the same SDK RPCs as the Agent.
//!
//! Only the Linux launcher evidence is replaced: the task is admitted through
//! the real Store admission and its launch/process facts use a fixture process
//! (`FixtureProcess`, test-support only). Crash cases:
//! (a) the child executed and its result is durable in the broker journal, but
//!     the parent never committed; the whole host restarts (new owner, broker,
//!     workers, sender) and recovery commits that original child once;
//! (b) the parent committed but the reply was lost before the child row was
//!     released; restart releases the row without a second commit or effect.
use super::*;
use crate::agent_sender::{CloudSender, StepProgress};
use qos_action_broker::enrolled_dispatch::{
    ChildCommit, ChildTicket, EnrolledDispatchResult, EnrolledStepService, EnrolledTaskParent,
    FixtureProcess, ParentFailure, ParentOwner, ParentReceipt, ParentRecord, ParentStep,
    VerifiedEnrolledDispatch,
};
use std::os::fd::IntoRawFd;
use ui::collaboration::{CollabEvent, CollabRequest};

const WRITER: &str = "so.tilo.word-processor";
const RECORD: &str = "tab:0000000000000001";

fn device() -> [u8; 32] {
    ed25519_dalek::SigningKey::from_bytes(&[8; 32])
        .verifying_key()
        .to_bytes()
}

/// Real certified workers with their startup publications committed.
async fn started(
    directory: &std::path::Path,
    owner: Arc<RuntimeOwner>,
) -> (
    AgentWorkers,
    mpsc::Receiver<PendingHostDocument>,
    mpsc::Receiver<NeighbourMsg>,
) {
    let (outbound, mut network) = mpsc::channel(64);
    let (mut workers, mut documents) = AgentWorkers::start(directory, owner, outbound).unwrap();
    workers.set_transport_ready(true);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        workers.pump().unwrap();
        drain(&mut workers, &mut documents, &mut network);
        if workers.publications().unwrap().ready
            && workers.broker.published_action_surfaces().iter().any(|s| {
                s.live
                    && s.app_id == WRITER
                    && s.route.record_id == RECORD
                    && s.actions.iter().any(|a| a.action == "insert-table")
            })
        {
            return (workers, documents, network);
        }
        if tokio::time::Instant::now() >= deadline {
            let p = workers.publications().unwrap();
            let s: Vec<_> = workers
                .broker
                .published_action_surfaces()
                .iter()
                .map(|s| {
                    (
                        s.live,
                        s.app_id.clone(),
                        s.route.record_id.clone(),
                        s.actions.len(),
                    )
                })
                .collect();
            panic!(
                "workers not ready: ready={} grants={} surfaces={s:?}",
                p.ready,
                p.grants.len()
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

/// Commit every queued worker document through the actual final door.
fn drain(
    workers: &mut AgentWorkers,
    documents: &mut mpsc::Receiver<PendingHostDocument>,
    network: &mut mpsc::Receiver<NeighbourMsg>,
) {
    while let Ok(pending) = documents.try_recv() {
        let owner = workers.owner.clone();
        let (complete, outcome) = crate::agent_documents::apply_worker_document(
            &owner,
            pending,
            std::time::Instant::now() + std::time::Duration::from_secs(5),
        );
        outcome.unwrap();
        workers.committed().unwrap();
        complete();
    }
    while network.try_recv().is_ok() {}
}

fn writer_blocks(owner: &RuntimeOwner) -> usize {
    let bytes = owner
        .store()
        .unwrap()
        .peer()
        .unwrap()
        .document(WRITER, RECORD)
        .unwrap();
    let document = ui::automerge::AutoCommit::load(&bytes).unwrap();
    let model = qdoc::model::open(&document, &ui::automerge::ROOT).unwrap();
    qdoc::model::blocks_in_order(&document, &model).len()
}

fn committed_steps(owner: &RuntimeOwner, invocation: uuid::Uuid) -> Vec<String> {
    let store = owner.store().unwrap();
    let Some(qos_peer_state::owner_commit::PrivateAgentState::V2(ledger)) =
        &store.state.private_agent_ledger
    else {
        panic!("private runtime ledger missing")
    };
    ledger
        .facts
        .keys()
        .filter(|key| key.starts_with(&format!("{invocation}:task_step_committed")))
        .cloned()
        .collect()
}

struct Task {
    invocation: uuid::Uuid,
    channel: String,
    client: ui::collaboration::CollaborationManager,
    _process: Arc<FixtureProcess>,
    acceptance: runtime_collab::EnrolledTaskAcceptance,
}

/// Admit, launch (fixture evidence), connect the enrolled Agent client,
/// dispatch through the production broker path and accept as the recipient.
async fn enrolled_task(
    workers: &mut AgentWorkers,
    documents: &mut mpsc::Receiver<PendingHostDocument>,
    network: &mut mpsc::Receiver<NeighbourMsg>,
    certified: bool,
) -> Task {
    let grants = workers.publications().unwrap().grants;
    enrolled_task_granted(workers, documents, network, certified, grants).await
}

/// [`enrolled_task`] with explicit retained grants (v1 tuples or v2 scopes).
async fn enrolled_task_granted(
    workers: &mut AgentWorkers,
    documents: &mut mpsc::Receiver<PendingHostDocument>,
    network: &mut mpsc::Receiver<NeighbourMsg>,
    certified: bool,
    grants: Vec<GrantAction>,
) -> Task {
    let invocation = uuid::Uuid::new_v4();
    let owner = workers.owner.clone();
    let task = {
        let mut store = owner.store().unwrap();
        let caller = store.agent_caller(device()).unwrap();
        store.enrolled_fixture_task(&caller, invocation, grants)
    };
    let process_facts = task.process.clone();
    let caller = task.intent.scope.caller.clone();
    let ids = task.ids.clone();
    let title = "Insert two tables".to_string();
    owner.install_fixture_attempt(task).unwrap();
    let (process, fixture) = FixtureProcess::new(process_facts.clone());
    let (broker_side, agent_side) = std::os::unix::net::UnixStream::pair().unwrap();
    let broker = workers.app_broker().unwrap();
    let channel = broker
        .install_fixture_enrolled_connection(process, broker_side, caller.clone())
        .unwrap();
    // The enrolled Agent presents its original caller's certified device.
    let key = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
    let presentation = runtime_collab::CertifiedJoinIdentity {
        workspace_id: workers.room.clone(),
        device_certificate: serde_json::to_vec(&caller).unwrap(),
        proof: key
            .sign(&runtime_identity::collaboration_join_signing_bytes(
                &workers.room,
            ))
            .to_bytes()
            .to_vec(),
    }
    .encode()
    .unwrap();
    let mut client = if certified {
        ui::collaboration::CollaborationManager::for_agent_route_fd_with_identity(
            agent_side.into_raw_fd(),
            "so.tilo.agent",
            workers.room.parse().unwrap(),
            None,
            process_facts.binding.agent.clone(),
            ui::collaboration::CertifiedEndpointIdentity::new(presentation, key),
        )
        .unwrap()
    } else {
        // The production managed Agent holds no caller key (a Cloud owner's
        // device key stays in the browser): it joins its enrolled descriptor
        // with an empty presentation, admitted from the host's binding.
        ui::collaboration::CollaborationManager::for_agent_route_fd(
            agent_side.into_raw_fd(),
            "so.tilo.agent",
            workers.room.parse().unwrap(),
            None,
            process_facts.binding.agent.clone(),
        )
        .unwrap()
    };
    assert!(client.send(CollabRequest::SubscribeEnrolledTasks { request_id: 1 }));
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while !broker.enrolled_recipient_ready(&channel) {
        workers.pump().unwrap();
        drain(workers, documents, network);
        let _ = client.poll();
        assert!(
            tokio::time::Instant::now() < deadline,
            "recipient not subscribed"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let envelope = runtime_collab::EnrolledTaskEnvelope {
        version: 1,
        message_id: ids.message_id.clone(),
        task_id: ids.task_id.clone(),
        run_id: ids.run_id.clone(),
        launch_id: ids.launch_id.clone(),
        runtime_epoch: process_facts.runtime.runtime_epoch.clone(),
        recipient_agent: ids.agent_id.clone(),
        channel_lease: process_facts.channel_id.clone(),
        history: None,
        task: runtime_collab::TaskRunEnvelope {
            version: 1,
            task_id: ids.task_id.clone(),
            title,
            run_id: 1,
            lease: uuid::Uuid::new_v4().to_string(),
            planning: runtime_collab::TaskRunPlanning::Model,
            action_context: serde_json::to_value(qos_actions::ActionContext {
                action_label: "Enrolled task".into(),
                group: Some(qos_actions::ActionGroup {
                    id: ids.task_id.clone(),
                    label: ids.task_id.clone(),
                    kind: "task".into(),
                }),
                ..Default::default()
            })
            .unwrap(),
        },
    };
    assert!(matches!(
        broker
            .dispatch_enrolled_task(invocation, &caller, envelope)
            .unwrap(),
        EnrolledDispatchResult::Sent(_)
    ));
    let mut accepted = None;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        workers.pump().unwrap();
        drain(workers, documents, network);
        for event in client.poll() {
            match event {
                CollabEvent::EnrolledTask(envelope) => {
                    let acceptance = runtime_collab::EnrolledTaskAcceptance {
                        version: 1,
                        message_id: envelope.message_id.clone(),
                        task_id: envelope.task_id.clone(),
                        run_id: envelope.run_id.clone(),
                        launch_id: envelope.launch_id.clone(),
                        runtime_epoch: envelope.runtime_epoch.clone(),
                        recipient_agent: envelope.recipient_agent.clone(),
                        channel_lease: envelope.channel_lease.clone(),
                        acceptance_sequence: 1,
                    };
                    assert!(client.send(CollabRequest::AcceptEnrolledTask {
                        request_id: 2,
                        acceptance: acceptance.clone(),
                    }));
                    accepted = Some(acceptance);
                }
                CollabEvent::EnrolledTaskReply {
                    request_id: 2,
                    result,
                } => {
                    result.unwrap();
                    return Task {
                        invocation,
                        channel,
                        client,
                        _process: fixture,
                        acceptance: accepted.take().expect("accepted envelope"),
                    };
                }
                _ => {}
            }
        }
        assert!(tokio::time::Instant::now() < deadline, "task not accepted");
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

fn step(task: &Task, index: u32) -> ParentStep {
    ParentStep::new(*task.invocation.as_bytes(), index).unwrap()
}

fn writer_peer(workers: &AgentWorkers) -> (u64, String) {
    workers
        .broker
        .published_action_surfaces()
        .into_iter()
        .filter(|s| s.live && s.app_id == WRITER && s.route.record_id == RECORD)
        .find_map(|s| {
            s.actions
                .iter()
                .find(|a| a.action == "insert-table")
                .map(|a| (s.peer_id, a.resource.clone()))
        })
        .expect("live Writer insert-table")
}

/// Drive a step through the real worker until its child is terminal in the
/// broker journal. `settle` false stops before any parent commit.
#[allow(
    clippy::too_many_arguments,
    reason = "explicit protocol inputs stay separate parameters"
)]
async fn drive(
    sender: &mut CloudSender,
    workers: &mut AgentWorkers,
    documents: &mut mpsc::Receiver<PendingHostDocument>,
    network: &mut mpsc::Receiver<NeighbourMsg>,
    dispatch: &VerifiedEnrolledDispatch,
    parent: &mut dyn ParentOwner,
    task: &Task,
    index: u32,
    settle: bool,
) -> Result<StepProgress, String> {
    let broker = workers.app_broker().unwrap();
    let (peer, resource) = writer_peer(workers);
    let intent =
        CloudSender::child_intent(&broker, dispatch, peer, &resource, "insert-table", None)
            .unwrap();
    let owner = workers
        .owner
        .store()
        .unwrap()
        .enrolled_child_owner(task.invocation)
        .unwrap();
    let mut progress = sender
        .step(
            &broker,
            dispatch,
            parent,
            step(task, index),
            &intent,
            &owner,
        )
        .map_err(|e| format!("{e:?}"))?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let StepProgress::Settled { .. } = progress {
            return Ok(progress);
        }
        workers.pump().unwrap();
        drain(workers, documents, network);
        let StepProgress::Pending(child) = progress else {
            unreachable!()
        };
        if !settle {
            let row = broker
                .observe_enrolled_child(&owner, child.host(), child.invocation())
                .unwrap()
                .expect("admitted child retained by the broker journal");
            let update: qos_actions::InvocationUpdate =
                serde_json::from_str(&row.update_json).unwrap();
            if update.is_terminal() {
                return Ok(StepProgress::Pending(child));
            }
        } else {
            progress = sender
                .observe(&broker, parent, step(task, index), &owner)
                .map_err(|e| format!("{e:?}"))?;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "child never terminal"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

/// Parent adapter that commits through the real enrolled owner, then loses
/// the reply (crash after the parent commit, before child release).
struct LoseCommitReply<'a>(&'a mut EnrolledTaskParent);
impl ParentOwner for LoseCommitReply<'_> {
    fn lookup(&self, step: ParentStep) -> Result<Option<ParentRecord>, ParentFailure> {
        self.0.lookup(step)
    }
    fn find_child(&self, child: ChildTicket) -> Result<Option<ParentRecord>, ParentFailure> {
        self.0.find_child(child)
    }
    fn bind(
        &mut self,
        step: ParentStep,
        child: ChildTicket,
        intent: [u8; 32],
    ) -> Result<ParentRecord, ParentFailure> {
        self.0.bind(step, child, intent)
    }
    fn commit(&mut self, commit: ChildCommit<'_>) -> Result<ParentReceipt, ParentFailure> {
        self.0.commit(commit)?;
        Err(ParentFailure::Uncertain(
            "injected crash after parent commit".into(),
        ))
    }
}

async fn restart(
    directory: &std::path::Path,
    workers: AgentWorkers,
) -> (
    Arc<RuntimeOwner>,
    AgentWorkers,
    mpsc::Receiver<PendingHostDocument>,
    mpsc::Receiver<NeighbourMsg>,
) {
    let mut workers = workers;
    workers
        .close()
        .expect("clean worker shutdown before restart");
    drop(workers);
    let owner = Arc::new(RuntimeOwner::open(directory, "agent-admission").unwrap());
    // Production runtime start: every accepted, unfinished task becomes
    // uncertain. Its bound steps must stay recoverable.
    owner.retain_restart_uncertainty().unwrap();
    let (workers, documents, network) = started(directory, owner.clone()).await;
    (owner, workers, documents, network)
}

async fn handoff(crash_after_commit: bool, certified: bool) {
    let (directory, owner) = super::tests::selected();
    let (mut workers, mut documents, mut network) = started(directory.path(), owner).await;
    let mut task = enrolled_task(&mut workers, &mut documents, &mut network, certified).await;
    let broker = workers.app_broker().unwrap();
    let dispatch = broker
        .enrolled_task_dispatch(&task.channel, task.invocation)
        .unwrap();
    let mut enrolled = EnrolledTaskParent::new(&dispatch).unwrap();
    let base = writer_blocks(&workers.owner);

    // Step 0 through the live step service the broker's `invokeTaskAction`
    // step path uses: bound, executed once, committed, released, answered
    // with the committed result and continuation.
    let (peer, resource) = writer_peer(&workers);
    let mut answer = workers
        .step_service
        .submit(qos_action_broker::enrolled_dispatch::EnrolledStepRequest {
            dispatch: dispatch.clone(),
            step: 0,
            peer,
            resource,
            action: "insert-table".into(),
            argument: None,
        })
        .unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let reply = loop {
        workers.pump().unwrap();
        drain(&mut workers, &mut documents, &mut network);
        if let Ok(reply) = answer.try_recv() {
            break reply.unwrap();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "step 0 never answered"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    };
    let update: qos_actions::InvocationUpdate = serde_json::from_str(&reply.update_json).unwrap();
    assert!(
        matches!(update, qos_actions::InvocationUpdate::Succeeded { .. }),
        "first child did not succeed: {}",
        reply.update_json
    );
    let continuation: serde_json::Value = serde_json::from_slice(&reply.continuation).unwrap();
    assert!(continuation.get("succeeded").is_some(), "{continuation}");
    // The parent owner's step records are immutable: an equal repeat returns
    // the retained record, anything different for the same step conflicts.
    let record = enrolled.lookup(step(&task, 0)).unwrap().unwrap();
    let committed = record.committed.clone().unwrap();
    assert_eq!(
        enrolled
            .bind(step(&task, 0), record.child, record.intent)
            .unwrap(),
        record
    );
    let other = ChildTicket::from_bytes(&[7; 48]).unwrap();
    assert_eq!(
        enrolled.bind(step(&task, 0), other, record.intent),
        Err(ParentFailure::Conflict)
    );
    assert_eq!(
        enrolled.find_child(record.child).unwrap(),
        Some(record.clone())
    );
    // Release the Store guard before calling into the broker.
    let child_owner = workers
        .owner
        .store()
        .unwrap()
        .enrolled_child_owner(task.invocation)
        .unwrap();
    let exact_json = broker
        .observe_enrolled_child(&child_owner, record.child.host(), record.child.invocation())
        .unwrap()
        .unwrap()
        .update_json;
    let repeat = ChildCommit {
        step: step(&task, 0),
        child: record.child,
        intent: record.intent,
        result: committed.result,
        result_json: &exact_json,
        continuation: &committed.continuation,
    };
    // The retained JSON is the sender row's exact bytes; the broker's
    // re-serialization may differ, so only the changed continuation is
    // asserted to conflict here.
    let changed = ChildCommit {
        continuation: b"different",
        ..repeat
    };
    assert_eq!(enrolled.commit(changed), Err(ParentFailure::Conflict));
    // The host recovery parent can never bind a new step.
    assert!(matches!(
        workers
            .owner
            .recovery_parent()
            .bind(step(&task, 5), other, record.intent),
        Err(ParentFailure::Unavailable(_))
    ));
    assert_eq!(committed_steps(&workers.owner, task.invocation).len(), 1);
    let after_first = writer_blocks(&workers.owner);
    assert!(after_first > base, "first child had no document effect");
    assert_eq!(committed_steps(&workers.owner, task.invocation).len(), 1);

    // Step 1: the child executes to a durable terminal result.
    let mut steps = workers.steps.take().expect("installed managed sender");
    if crash_after_commit {
        let mut lossy = LoseCommitReply(&mut enrolled);
        let result = drive(
            &mut steps.sender,
            &mut workers,
            &mut documents,
            &mut network,
            &dispatch,
            &mut lossy,
            &task,
            1,
            true,
        )
        .await;
        assert!(
            result.is_err(),
            "lost commit reply must not report settlement"
        );
        assert_eq!(
            committed_steps(&workers.owner, task.invocation).len(),
            2,
            "parent commit must be durable before the lost reply"
        );
    } else {
        let pending = drive(
            &mut steps.sender,
            &mut workers,
            &mut documents,
            &mut network,
            &dispatch,
            &mut enrolled,
            &task,
            1,
            false,
        )
        .await
        .unwrap();
        assert!(matches!(pending, StepProgress::Pending(_)));
        assert_eq!(committed_steps(&workers.owner, task.invocation).len(), 1);
        // The task cannot record success over the bound, uncommitted child.
        assert!(task.client.send(CollabRequest::ReportEnrolledTask {
            request_id: 3,
            acceptance: task.acceptance.clone(),
            sequence: 1,
            update: runtime_collab::TaskRunUpdate::Succeeded {
                message: "Done".into(),
            },
        }));
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        'refused: loop {
            workers.pump().unwrap();
            drain(&mut workers, &mut documents, &mut network);
            for event in task.client.poll() {
                if let CollabEvent::EnrolledTaskReply {
                    request_id: 3,
                    result,
                } = event
                {
                    assert!(result.is_err(), "success accepted over an open step");
                    break 'refused;
                }
            }
            assert!(tokio::time::Instant::now() < deadline, "report unanswered");
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }
    let after_second = writer_blocks(&workers.owner);
    let effect = after_first - base;
    assert_eq!(
        after_second,
        after_first + effect,
        "second child effect missing"
    );

    // Crash: the sender, parent, Agent connection and the whole host go away.
    drop(enrolled);
    drop(dispatch);
    drop(steps);
    drop(task.client);
    let invocation = task.invocation;
    let (owner, mut workers, mut documents, mut network) = restart(directory.path(), workers).await;
    // Worker startup installed the sender and reconciled it with the
    // recovery parent (no live enrolled dispatch exists any more).
    drain(&mut workers, &mut documents, &mut network);
    let recovered = &workers
        .steps
        .as_ref()
        .expect("installed managed sender")
        .recovered;
    assert_eq!(
        recovered.len(),
        1,
        "exactly the original second child recovers"
    );
    let (recovered_step, StepProgress::Settled { continuation, .. }) = &recovered[0] else {
        panic!("recovered child not settled: {recovered:?}")
    };
    assert_eq!(recovered_step.index(), 1);
    let continuation: serde_json::Value = serde_json::from_slice(continuation).unwrap();
    assert!(continuation.get("succeeded").is_some());
    assert_eq!(committed_steps(&owner, invocation).len(), 2);
    {
        let store = owner.store().unwrap();
        let Some(qos_peer_state::owner_commit::PrivateAgentState::V2(ledger)) =
            &store.state.private_agent_ledger
        else {
            panic!("private runtime ledger missing")
        };
        assert!(
            ledger
                .facts
                .contains_key(&format!("{invocation}:task_unknown")),
            "restart must have made the unfinished task uncertain"
        );
    }
    assert_eq!(
        writer_blocks(&owner),
        after_second,
        "recovery re-executed a child or lost its effect"
    );
    // A second restart finds nothing left to recover and changes nothing.
    let (owner, workers, _documents, _network) = restart(directory.path(), workers).await;
    assert!(
        workers
            .steps
            .as_ref()
            .expect("installed managed sender")
            .recovered
            .is_empty()
    );
    assert_eq!(committed_steps(&owner, invocation).len(), 2);
    assert_eq!(writer_blocks(&owner), after_second);
    let _ = task._process;
}

#[tokio::test(flavor = "current_thread")]
async fn enrolled_two_step_handoff_recovers_child_after_crash_before_parent_commit() {
    handoff(false, true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn enrolled_two_step_handoff_releases_child_after_crash_after_parent_commit() {
    handoff(true, true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn enrolled_agent_without_caller_key_joins_by_host_binding() {
    handoff(false, false).await;
}

/// Scoped grants (security review MUST-1/2/4/6/10, T1/T6/T20) through the
/// real Store step check with a real enrolled dispatch witness.
#[tokio::test(flavor = "current_thread")]
async fn scoped_grants_authorize_steps_by_record_class_and_revocation() {
    use qos_agent_control_protocol::{
        GrantScope, OperationClass, ScopeBudgets, decode_scope_carrier, scope_carrier,
    };
    let (directory, owner) = super::tests::selected();
    let (mut workers, mut documents, mut network) = started(directory.path(), owner).await;
    let live: Vec<_> = workers
        .broker
        .published_action_surfaces()
        .into_iter()
        .filter(|s| s.live && s.route.cluster_id == workers.room)
        .collect();
    let scoped = publication_scopes(&live).unwrap();
    assert!(!scoped.is_empty() && scoped.len() <= qos_agent_control_protocol::MAX_GRANT_SCOPES);
    let writer = scoped
        .iter()
        .map(|g| decode_scope_carrier(g).unwrap())
        .find(|s| s.app_id == WRITER && s.covers_record(RECORD))
        .expect("default Writer scope for the open record");
    assert_eq!(
        writer.classes,
        [
            OperationClass::Document,
            OperationClass::View,
            OperationClass::Query
        ]
    );
    assert!(consent_label(&scoped).unwrap().contains("It cannot"));
    let scope_only = |scope: &GrantScope| vec![scope_carrier(scope).unwrap()];

    let task = enrolled_task_granted(
        &mut workers,
        &mut documents,
        &mut network,
        true,
        scope_only(&writer),
    )
    .await;
    let broker = workers.app_broker().unwrap();
    let dispatch = broker
        .enrolled_task_dispatch(&task.channel, task.invocation)
        .unwrap();
    let check = |record: &str, action: &str| {
        workers.owner.store().unwrap().authorize_agent_task_action(
            &dispatch,
            WRITER,
            record,
            "word-processor.document",
            action,
        )
    };
    for action in ["type-text", "insert-table", "move-caret", "read-text"] {
        check(RECORD, action).unwrap_or_else(|e| panic!("{action}: {e}"));
    }
    // Host filesystem effects, unlisted ops, another record, a reserved
    // spelling and another app are all outside the frozen scope.
    for (record, action) in [
        (RECORD, "save"),
        (RECORD, "open"),
        (RECORD, "delete-style"),
        (RECORD, "accept-all"),
        ("tab:0000000000000009", "type-text"),
        ("@qos:package-primary", "type-text"),
        ("*in-workspace", "type-text"),
    ] {
        assert!(check(record, action).is_err(), "{record} {action}");
    }
    assert!(
        workers
            .owner
            .store()
            .unwrap()
            .authorize_agent_task_action(
                &dispatch,
                "so.tilo.chat",
                RECORD,
                "chat.channel:c",
                "post"
            )
            .is_err()
    );
    // Owner revocation: the next step is refused.
    {
        let mut store = workers.owner.store().unwrap();
        let id = crate::agent_runtime_owner::AgentControlId::new(task.invocation).unwrap();
        let caller = store.agent_caller(device()).unwrap().certificate().clone();
        let mut stranger = caller.clone();
        stranger.user_public_key = [0xee; 32];
        assert!(store.revoke_agent_task(&stranger, &id).is_err());
        store.revoke_agent_task(&caller, &id).unwrap();
        // The durable audit records the step decisions for the owner.
        let audit = store.agent_task_audit(&caller, &id).unwrap();
        assert!(
            audit
                .iter()
                .any(|entry| entry["action"] == "type-text" && entry["allowed"] == true)
        );
        assert!(
            audit
                .iter()
                .any(|entry| entry["action"] == "save" && entry["allowed"] == false)
        );
        assert!(store.agent_task_audit(&stranger, &id).is_err());
    }
    assert!(check(RECORD, "type-text").is_err());
    drop(dispatch);
    drop(task);

    // A view-only scope cannot edit; an expired wall clock stops the task.
    let view_only = GrantScope {
        classes: vec![OperationClass::View],
        ..writer.clone()
    };
    let task = enrolled_task_granted(
        &mut workers,
        &mut documents,
        &mut network,
        true,
        scope_only(&view_only),
    )
    .await;
    let dispatch = broker
        .enrolled_task_dispatch(&task.channel, task.invocation)
        .unwrap();
    let store = workers.owner.store().unwrap();
    store
        .authorize_agent_task_action(
            &dispatch,
            WRITER,
            RECORD,
            "word-processor.document",
            "move-caret",
        )
        .unwrap();
    assert!(
        store
            .authorize_agent_task_action(
                &dispatch,
                WRITER,
                RECORD,
                "word-processor.document",
                "type-text"
            )
            .is_err()
    );
    drop(store);
    drop(dispatch);
    drop(task);

    let short = GrantScope {
        budgets: Some(ScopeBudgets {
            wall_clock_secs: 1,
            ..ScopeBudgets::default()
        }),
        ..writer.clone()
    };
    let task = enrolled_task_granted(
        &mut workers,
        &mut documents,
        &mut network,
        true,
        scope_only(&short),
    )
    .await;
    let dispatch = broker
        .enrolled_task_dispatch(&task.channel, task.invocation)
        .unwrap();
    let authorize = || {
        workers.owner.store().unwrap().authorize_agent_task_action(
            &dispatch,
            WRITER,
            RECORD,
            "word-processor.document",
            "type-text",
        )
    };
    authorize().unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
    let refused = authorize().unwrap_err().to_string();
    assert!(refused.contains("wall-clock"), "{refused}");
}

/// v1 exact tuples on a v2 host keep exactly today's allow set (T20).
#[tokio::test(flavor = "current_thread")]
async fn v1_tuple_grants_keep_their_exact_allow_set() {
    let (directory, owner) = super::tests::selected();
    let (mut workers, mut documents, mut network) = started(directory.path(), owner).await;
    let (_, resource) = writer_peer(&workers);
    let tuple = GrantAction {
        app_id: WRITER.into(),
        record_id: RECORD.into(),
        resource: resource.clone(),
        action: "insert-table".into(),
    };
    let task = enrolled_task_granted(
        &mut workers,
        &mut documents,
        &mut network,
        true,
        vec![tuple],
    )
    .await;
    let broker = workers.app_broker().unwrap();
    let dispatch = broker
        .enrolled_task_dispatch(&task.channel, task.invocation)
        .unwrap();
    let store = workers.owner.store().unwrap();
    store
        .authorize_agent_task_action(&dispatch, WRITER, RECORD, &resource, "insert-table")
        .unwrap();
    for action in ["type-text", "append-paragraph", "save"] {
        assert!(
            store
                .authorize_agent_task_action(&dispatch, WRITER, RECORD, &resource, action)
                .is_err(),
            "{action}"
        );
    }
}

/// Agent task views follow the live tasks: joined on covered records only,
/// dropped when the task is no longer active (MUST-12).
#[tokio::test(flavor = "current_thread")]
async fn agent_participants_follow_live_task_scopes() {
    let (directory, owner) = super::tests::selected();
    let (mut workers, _documents, _network) = started(directory.path(), owner).await;
    let live: Vec<_> = workers
        .broker
        .published_action_surfaces()
        .into_iter()
        .filter(|s| s.live && s.route.cluster_id == workers.room)
        .collect();
    let scoped = publication_scopes(&live).unwrap();
    let task = "11111111-1111-4111-8111-111111111111".to_string();
    workers
        .sync_agent_participants(&[ActiveAgentTask {
            task: task.clone(),
            agent: "agent-1".into(),
            name: "Tide".into(),
            grants: scoped,
        }])
        .unwrap();
    assert_eq!(
        workers.task_agents.get(&task).map(String::as_str),
        Some("agent-1")
    );
    let participant = format!("agent-task:{task}");
    assert!(workers.participants.contains(&(
        WRITER.to_owned(),
        RECORD.to_owned(),
        participant.clone()
    )));
    assert!(
        workers
            .participants
            .iter()
            .all(|(_, _, p)| *p == participant)
    );
    workers.sync_agent_participants(&[]).unwrap();
    assert!(workers.participants.is_empty() && workers.task_agents.is_empty());
}

#[derive(Clone, Copy, Debug)]
enum EnrolledJoin {
    /// The production managed Agent: agent kind, bound handle, no presentation.
    Bound,
    /// A Writer channel on the enrolled descriptor.
    WrongRoute,
    /// A person principal on the enrolled descriptor.
    Person,
    /// An agent handle other than the launch-bound agent.
    OtherAgent,
    /// A non-empty presentation whose proof was signed by a foreign key.
    ForeignPresentation,
    /// The bound managed process has exited before the join.
    Exited,
}

/// One enrolled join attempt over the real broker on a fixture descriptor.
async fn enrolled_join(variant: EnrolledJoin) -> bool {
    let (directory, owner) = super::tests::selected();
    let (workers, _documents, _network) = started(directory.path(), owner).await;
    let invocation = uuid::Uuid::new_v4();
    let grants = workers.publications().unwrap().grants;
    let task = {
        let mut store = workers.owner.store().unwrap();
        let caller = store.agent_caller(device()).unwrap();
        store.enrolled_fixture_task(&caller, invocation, grants)
    };
    let facts = task.process.clone();
    let caller = task.intent.scope.caller.clone();
    workers.owner.install_fixture_attempt(task).unwrap();
    let (process, fixture) = FixtureProcess::new(facts.clone());
    let (broker_side, agent_side) = std::os::unix::net::UnixStream::pair().unwrap();
    workers
        .app_broker()
        .unwrap()
        .install_fixture_enrolled_connection(process, broker_side, caller.clone())
        .unwrap();
    let room: uuid::Uuid = workers.room.parse().unwrap();
    let agent = facts.binding.agent.clone();
    let fd = agent_side.into_raw_fd();
    use ui::collaboration::CollaborationManager as Manager;
    let joined = match variant {
        EnrolledJoin::Bound => Manager::for_agent_route_fd(fd, "so.tilo.agent", room, None, agent),
        EnrolledJoin::Exited => {
            fixture.exit();
            Manager::for_agent_route_fd(fd, "so.tilo.agent", room, None, agent)
        }
        EnrolledJoin::WrongRoute => Manager::for_agent_route_fd(fd, WRITER, room, None, agent),
        EnrolledJoin::Person => Manager::for_route_fd(fd, "so.tilo.agent", room, None, None),
        EnrolledJoin::OtherAgent => {
            Manager::for_agent_route_fd(fd, "so.tilo.agent", room, None, "someone-else")
        }
        EnrolledJoin::ForeignPresentation => {
            let foreign = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
            let presentation = runtime_collab::CertifiedJoinIdentity {
                workspace_id: workers.room.clone(),
                device_certificate: serde_json::to_vec(&caller).unwrap(),
                proof: foreign
                    .sign(&runtime_identity::collaboration_join_signing_bytes(
                        &workers.room,
                    ))
                    .to_bytes()
                    .to_vec(),
            }
            .encode()
            .unwrap();
            Manager::for_agent_route_fd_with_identity(
                fd,
                "so.tilo.agent",
                room,
                None,
                agent,
                ui::collaboration::CertifiedEndpointIdentity::new(presentation, foreign),
            )
        }
    };
    let _ = fixture;
    joined.is_ok()
}

#[tokio::test(flavor = "current_thread")]
async fn enrolled_join_admits_only_the_bound_agent_on_its_own_route() {
    assert!(
        enrolled_join(EnrolledJoin::Bound).await,
        "bound managed agent must join"
    );
    for variant in [
        EnrolledJoin::WrongRoute,
        EnrolledJoin::Person,
        EnrolledJoin::OtherAgent,
        EnrolledJoin::ForeignPresentation,
        EnrolledJoin::Exited,
    ] {
        assert!(!enrolled_join(variant).await, "{variant:?} must be refused");
    }
}

/// Regression (lab h1 2026-09-24: the agent catalogue had no Writer ops):
/// the real Writer and Chat worker publications → v1 grants → the enrolled
/// catalogue filter must offer Writer `type-text` and `append-paragraph`
/// (and Chat `post`) on the live surfaces.
#[tokio::test(flavor = "current_thread")]
async fn real_worker_publication_offers_writer_ops_through_the_catalogue_filter() {
    let (directory, owner) = super::tests::selected();
    let (workers, _documents, _network) = started(directory.path(), owner).await;
    let grants = workers.publications().unwrap().grants;
    // Default member-rights scopes name every live delegable record.
    let scopes = qos_agent_control_protocol::scopes_from_grants(&grants).unwrap();
    assert!(
        scopes
            .iter()
            .any(|s| s.app_id == WRITER && s.covers_record(RECORD))
    );
    assert!(
        scopes
            .iter()
            .any(|s| s.app_id == "so.tilo.chat" && s.covers_record("@qos:package-primary"))
    );
    // The catalogue the enrolled agent reads is the live surfaces filtered
    // by its grants (same JSON shape as the broker's catalogue).
    let surfaces: Vec<_> = workers
        .broker
        .published_action_surfaces()
        .into_iter()
        .filter(|s| s.live && s.route.cluster_id == workers.room)
        .map(|s| {
            serde_json::json!({"peer_id": s.peer_id, "app_id": s.app_id,
                "record_id": s.route.record_id, "live": s.live,
                "actions": s.actions.iter().map(|a| serde_json::json!({
                    "resource": a.resource, "action": a.action,
                    "resource_kind": a.resource_kind,
                    "operation_kind": a.operation_kind, "contract": a.contract,
                })).collect::<Vec<_>>()})
        })
        .collect();
    let catalogue = serde_json::json!({ "surfaces": surfaces }).to_string();
    let filtered: serde_json::Value = serde_json::from_str(
        &qos_action_broker::agent_control::scoped::filter_scoped_catalogue(&catalogue, &grants)
            .unwrap(),
    )
    .unwrap();
    let listed = |app: &str, action: &str| {
        filtered["surfaces"].as_array().unwrap().iter().any(|s| {
            s["app_id"] == app
                && s["actions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|a| a["action"] == action)
        })
    };
    assert!(listed(WRITER, "type-text") && listed(WRITER, "append-paragraph"));
    assert!(listed("so.tilo.chat", "post"));
}

/// The same regression after the document has agent-written content and the
/// host restarted (the lab sequence: an agent wrote a paragraph, the host
/// was upgraded, a new agent spawned). Root cause: with any text in the
/// first block, Writer's participant-host catalogue schema publishes
/// `new-paragraph` as a zero-parameter Document op whose immediate envelope
/// is the unregistered invalid-arguments sentinel; the Writer component then
/// fails contract validation and ui-core silently drops its whole surface.
#[tokio::test(flavor = "current_thread")]
async fn writer_ops_stay_offered_after_content_and_restart() {
    let (directory, owner) = super::tests::selected();
    let (mut workers, mut documents, mut network) = started(directory.path(), owner).await;
    let task = enrolled_task(&mut workers, &mut documents, &mut network, true).await;
    let broker = workers.app_broker().unwrap();
    let dispatch = broker
        .enrolled_task_dispatch(&task.channel, task.invocation)
        .unwrap();
    let (peer, resource) = writer_peer(&workers);
    let mut answer = workers
        .step_service
        .submit(qos_action_broker::enrolled_dispatch::EnrolledStepRequest {
            dispatch: dispatch.clone(),
            step: 0,
            peer,
            resource,
            action: "append-paragraph".into(),
            argument: Some("The ocean is a vast, welcoming world.".into()),
        })
        .unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let reply = loop {
        workers.pump().unwrap();
        drain(&mut workers, &mut documents, &mut network);
        if let Ok(reply) = answer.try_recv() {
            break reply.unwrap();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "step never answered"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    };
    assert!(
        reply.update_json.contains("\"succeeded\""),
        "{}",
        reply.update_json
    );
    drop(dispatch);
    drop(task);
    {
        let bytes = workers
            .owner
            .store()
            .unwrap()
            .peer()
            .unwrap()
            .document(WRITER, RECORD)
            .unwrap();
        let doc = ui::automerge::AutoCommit::load(&bytes).unwrap();
        let schema = word_processor::actions::catalogue_schema(&doc);
        let mut registry = qos_actions::ActionRegistry::new();
        word_processor::actions::register(&mut registry);
        let violations = qos_actions::validate_surface_contracts(&schema, &registry);
        assert!(
            violations.is_empty(),
            "Writer catalogue schema fails contract validation, so its surface is dropped: {violations:?}"
        );
    }
    let (_owner, workers, _documents, _network) = restart(directory.path(), workers).await;
    let grants = workers.publications().unwrap().grants;
    let scopes = qos_agent_control_protocol::scopes_from_grants(&grants).unwrap();
    assert!(
        scopes
            .iter()
            .any(|s| s.app_id == WRITER && s.covers_record(RECORD)),
        "Writer record missing after content + restart: {scopes:?}"
    );
}

/// An agent task's participant gets a labelled caret on the Writer record it
/// may edit, from the host's own sync (lab: no participant presence reached
/// agent_live). Root cause: `BoundAppWidget` does not override
/// `participant_presence`, so the Writer resolves its participant views
/// against the outer App document instead of the component's record
/// document and finds no block (fix: ui-core-app_widget-participant-presence.diff).
#[tokio::test(flavor = "current_thread")]
async fn agent_participant_has_labelled_writer_presence() {
    let (directory, owner) = super::tests::selected();
    let (mut workers, mut documents, mut network) = started(directory.path(), owner).await;
    let grants = workers.publications().unwrap().grants;
    // Content first, so the document end is a non-empty block.
    let task = enrolled_task(&mut workers, &mut documents, &mut network, true).await;
    let broker = workers.app_broker().unwrap();
    let dispatch = broker
        .enrolled_task_dispatch(&task.channel, task.invocation)
        .unwrap();
    let (peer, resource) = writer_peer(&workers);
    let mut answer = workers
        .step_service
        .submit(qos_action_broker::enrolled_dispatch::EnrolledStepRequest {
            dispatch: dispatch.clone(),
            step: 0,
            peer,
            resource,
            action: "append-paragraph".into(),
            argument: Some("The ocean is wide.".into()),
        })
        .unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        workers.pump().unwrap();
        drain(&mut workers, &mut documents, &mut network);
        if let Ok(reply) = answer.try_recv() {
            reply.unwrap();
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "step never answered"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let active = [ActiveAgentTask {
        task: "11111111-1111-4111-8111-111111111111".into(),
        agent: "agent-1".into(),
        name: "Tide".into(),
        grants,
    }];
    // Two publish ticks: join, then the label once the view exists.
    for _ in 0..2 {
        workers.sync_agent_participants(&active).unwrap();
        workers.pump().unwrap();
        drain(&mut workers, &mut documents, &mut network);
    }
    let writer = workers
        .workers
        .iter()
        .find(|w| w.app == WRITER && w.record == RECORD)
        .unwrap();
    let presence = writer.runner.participant_presence();
    assert!(
        presence.iter().any(|p| p.participant.as_str()
            == "agent-task:11111111-1111-4111-8111-111111111111"
            && p.label.as_deref() == Some("Tide")),
        "{presence:?}"
    );
}

#[path = "participant_stream_tests.rs"]
mod participant_stream_tests;

#[path = "open_app_tests.rs"]
mod open_app_tests;
