//! Exercise the connection owner's production routing with real task threads.
use super::*;
use crate::model::Chooser;
use crate::session::{Config, agent_loop, outstanding};
use uuid::Uuid;

struct Peer(mpsc::Sender<CollabRequest>);

impl ui::collaboration::PeerTransport for Peer {
    fn is_connected(&self) -> bool {
        true
    }

    fn send(&mut self, request: CollabRequest) -> Result<(), String> {
        self.0.send(request).map_err(|error| error.to_string())
    }

    fn poll(&mut self) -> Vec<CollabEvent> {
        Vec::new()
    }
}

fn owner() -> (Session, mpsc::Receiver<CollabRequest>) {
    let (tx, rx) = mpsc::channel();
    let cfg = Config {
        agent_id: "agent-1".into(),
        endpoint: None,
        endpoint_identity: None,
        app_id: "so.tilo.agent".into(),
        cluster: Uuid::from_u128(1),
        record: None,
        name: "agent-1".into(),
        model: false,
        model_timeout: Duration::from_secs(1),
        permissions: ui::permissions::PermissionContext::agent_granted_at(
            "agent-1",
            &[],
            ui::permissions::KindSet::READ_UNDOABLE,
        ),
        content: tilo_content::ContentStore::memory(),
        companions: Vec::new(),
        provider: None,
    };
    let session = Session {
        chooser: Chooser::new(false, cfg.model_timeout),
        cfg,
        link: Link::for_task(Peer(tx)),
        native_model: None,
        managed: false,
        protected_model: false,
        task_deadline: None,
        native_task: None,
        broker_surfaces: Vec::new(),
        next_broker_request: 1,
        awaiting_companions: Vec::new(),
        broker_revision: 0,
        catalogue_cache: None,
        turn: None,
        memory: agent_loop::Memory::default(),
        outstanding: outstanding::Outstanding::default(),
    };
    (session, rx)
}

fn run(id: u64) -> runtime_collab::TaskRunEnvelope {
    let task_id = format!("task:{id}");
    runtime_collab::TaskRunEnvelope {
        paused: None,
        version: 1,
        task_id: task_id.clone(),
        title: "Review draft".into(),
        instruction: None,
        subject: None,
        run_id: id,
        lease: format!("native:{id:032x}:0"),
        planning: runtime_collab::TaskRunPlanning::Model,
        presentation_version: None,
        execution_budget: None,
        action_context: serde_json::json!({
            "purpose": "Review draft",
            "group": {"id": task_id, "label": "Review draft", "kind": "task"}
        }),
    }
}

#[test]
fn two_native_tasks_share_connection_without_sharing_reply_ids_or_fallback() {
    drive(false);
}

#[test]
fn parent_waiting_for_child_report_does_not_block_same_worker_child() {
    drive(true);
}

fn acknowledge(tasks: &mut Tasks, request_id: u64) {
    assert!(tasks.route(&CollabEvent::TaskRunReportResult {
        request_id,
        result: Ok(()),
    }));
}

fn drive(hold_parent: bool) {
    let (mut owner, requests) = owner();
    let mut tasks = Tasks::new();
    tasks.start(&owner, run(1)).unwrap();
    if !hold_parent {
        tasks.start(&owner, run(2)).unwrap();
    }
    // Re-delivery must not spawn a duplicate planner.
    tasks.start(&owner, run(1)).unwrap();
    assert_eq!(tasks.workers.len(), if hold_parent { 1 } else { 2 });
    let mut child_started = !hold_parent;
    let deadline = std::time::Instant::now() + Duration::from_secs(4);
    let mut ids = std::collections::HashSet::new();
    let mut failed = std::collections::HashSet::new();
    let mut held = None;
    let mut child_finished = false;
    while !tasks.workers.is_empty() {
        assert!(std::time::Instant::now() < deadline, "task routing stalled");
        tasks.pump(&mut owner).unwrap();
        // A pushed empty catalogue is still an observed catalogue. Broadcast
        // through the real router rather than mutating task thread state.
        tasks.route(&CollabEvent::ActionCatalogue {
            surfaces: Vec::new(),
        });
        for request in requests.try_iter() {
            match request {
                CollabRequest::ReportTaskRun {
                    request_id: Some(request_id),
                    run_id,
                    update,
                    ..
                } => {
                    assert!(ids.insert(request_id), "connection request id reused");
                    if matches!(update, runtime_collab::TaskRunUpdate::Failed { .. }) {
                        let runtime_collab::TaskRunUpdate::Failed { message } = update else {
                            unreachable!();
                        };
                        assert!(message.contains("model"), "unexpected failure: {message}");
                        assert!(
                            !message.contains("Rules-only"),
                            "model task fell back to rules"
                        );
                        failed.insert(run_id);
                        if run_id == 2 {
                            if hold_parent {
                                assert!(held.is_some(), "parent must still be waiting for child");
                            }
                            child_finished = true;
                        }
                    }
                    if hold_parent && run_id == 1 && !child_finished {
                        assert!(held.replace(request_id).is_none());
                    } else {
                        acknowledge(&mut tasks, request_id);
                    }
                }
                CollabRequest::TaskRunCancellation { request_id, .. } => {
                    assert!(ids.insert(request_id));
                    assert!(tasks.route(&CollabEvent::TaskRunCancellation {
                        request_id,
                        result: Ok(false),
                    }));
                }
                CollabRequest::InvokeTaskAction { .. } | CollabRequest::NativeModel { .. } => {
                    panic!("missing-provider model task must fail without invoking an action")
                }
                _ => {}
            }
        }
        if held.is_some() && !child_started {
            tasks.start(&owner, run(2)).unwrap();
            assert_eq!(tasks.workers.len(), 2);
            child_started = true;
        }
        if child_finished && let Some(request_id) = held.take() {
            acknowledge(&mut tasks, request_id);
        }
        std::thread::yield_now();
    }
    assert_eq!(failed, std::collections::HashSet::from([1, 2]));
    assert!(held.is_none());
    assert!(tasks.replies.is_empty());
    assert!(tasks.model_replies.is_empty());
    assert!(tasks.invocations.is_empty());
}
