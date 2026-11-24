//! A `Notes` workspace whose host also keeps its runs, as every workspace
//! host does: each change is a run of the member that made it, a listing
//! answers the runs of the member it is asked for, and `history task` reads
//! them beside the steps of this task.
use super::*;
use tilo_actions::remote::workspace_invocations::{
    Invocation, InvocationBy, InvocationChange, InvocationStatus,
};

/// [`Notes`] with the runs a workspace host keeps of what happened in it.
#[derive(Clone)]
pub(super) struct KeepingRuns {
    notes: Notes,
    /// The runs the workspace keeps, newest last.
    runs: Arc<Mutex<Vec<Invocation>>>,
}

impl KeepingRuns {
    /// `notes` as a workspace whose host keeps the runs its changes make.
    pub(super) fn of(notes: Notes) -> Self {
        Self {
            notes,
            runs: Arc::default(),
        }
    }

    /// Keep the run a change of `request` makes, attributed to the member
    /// `caller` acts as.
    fn note(&self, caller: &Caller, request: &InvokeRequest) {
        if matches!(
            request.target.address.action.as_str(),
            "read" | "history" | "slow" | "start"
        ) {
            return;
        }
        let action = request.target.address.action.clone();
        let acting = caller.acting();
        let mut kept = self.runs.lock().unwrap();
        let started_ms = i64::try_from(kept.len()).unwrap_or(i64::MAX);
        kept.push(Invocation {
            id: Some(request.invocation.0.to_string()),
            by: InvocationBy {
                name: acting.agent_id().unwrap_or("someone").to_owned(),
                kind: if acting.is_agent() { "agent" } else { "person" }.to_owned(),
                id: acting.member_id().map(|member| member.id().to_owned()),
            },
            status: InvocationStatus::Ended,
            started_ms,
            ended_ms: Some(started_ms),
            action: Some(format!("notes#{action}")),
            summary: Some(action),
            changes: 1,
            records: vec![InvocationChange {
                app: "notes".into(),
                app_id: APP.into(),
                record: String::new(),
            }],
            recorded: true,
            remembered: true,
            ..Invocation::default()
        });
        drop(kept);
    }

    /// The workspace's runs, newest first, as its host reads them through the record API.
    fn reading(&self) -> serde_json::Value {
        let mut runs = self.runs.lock().unwrap().clone();
        runs.sort_by_key(|run| std::cmp::Reverse(run.started_ms));
        serde_json::json!({"items": runs, "total": runs.len(), "waiting": 0})
    }
}

/// The record reader of [`KeepingRuns`]: the workspace's invocations, whole or one by its id.
struct RunsRead {
    runs: Arc<Mutex<Vec<Invocation>>>,
}

impl tilo_actions::remote::ResourceReads for RunsRead {
    // Each run is noted as the call is taken, so the next read sees it without a wake.
    fn watch(
        &self,
        _: &Caller,
        _: &tilo_actions::remote::ResourceAddress,
        _: &tilo_actions::remote::ResourceWake,
    ) -> Result<(), ActionCallError> {
        Ok(())
    }

    fn read(
        &self,
        _: &Caller,
        address: &tilo_actions::remote::ResourceAddress,
    ) -> Result<tilo_actions::remote::ResourceCopy, ActionCallError> {
        let mut runs = self.runs.lock().unwrap().clone();
        runs.sort_by_key(|run| std::cmp::Reverse(run.started_ms));
        if address.record.is_empty() {
            return Ok(tilo_actions::remote::ResourceCopy::reading(
                serde_json::json!({"items": runs, "total": runs.len(), "waiting": 0}),
                None,
            ));
        }
        Ok(runs
            .into_iter()
            .find(|run| run.id.as_deref() == Some(address.record.as_str()))
            .map_or(tilo_actions::remote::ResourceCopy::Absent, |one| {
                tilo_actions::remote::ResourceCopy::reading(
                    serde_json::to_value(one).unwrap_or_default(),
                    None,
                )
            }))
    }

    fn answer_read(
        self: std::sync::Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: tilo_actions::remote::ResourceReadRequest,
    ) -> Pending<tilo_actions::remote::RemoteActionMessage> {
        tilo_actions::remote::answer_read(self, caller, correlation, read)
    }
}

impl ActionProvider for KeepingRuns {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        self.notes.query(caller, query)
    }

    fn here(&self, caller: &Caller) -> Option<Vec<tilo_actions::remote::Here>> {
        self.notes.here(caller)
    }

    fn resource_search(&self) -> Option<std::sync::Arc<dyn tilo_actions::remote::ResourceSearch>> {
        self.notes.resource_search()
    }

    fn resource_reads(&self) -> Option<std::sync::Arc<dyn tilo_actions::remote::ResourceReads>> {
        Some(std::sync::Arc::new(RunsRead {
            runs: Arc::clone(&self.runs),
        }))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.note(caller, &request);
        self.notes.invoke(caller, request)
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        self.notes.observe(caller, id)
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        self.notes.cancel(caller, id)
    }
}

tilo_test_env::scenario! {
    fn history_task_reads_this_agents_runs_across_its_sessions;

    phase do_calls_are_part_of_the_history;
    // A `tilo_do` call is a run of the agent's, which `history task` reads
    // from the workspace, not only the programs this process ran.
    {
        let host = KeepingRuns::of(Notes::new(true));
        run(async {
            let server = agent(&host);
            let (ran, failed) = tool(
                &server,
                "tilo_do",
                json!({"path":"notes/Todo/body#append-paragraph","args":{"text":"milk"}}),
            )
            .await;
            assert!(!failed, "{ran}");
            let (history, failed) = history(&server).await;
            assert!(!failed, "{history}");
            assert!(
                history.contains("runs by this agent in this workspace, newest first (1 run)"),
                "{history}"
            );
            assert!(
                history.contains("1 change in notes: append-paragraph"),
                "{history}"
            );
            // The reading carries the workspace's runs; this agent's own are what it reads.
            assert_eq!(host.reading()["total"], 1, "{:?}", host.reading());
        });
    }

    phase a_later_session_of_the_same_agent_reads_the_earlier_one;
    {
        let host = KeepingRuns::of(Notes::new(true));
        run(async {
            let first = agent(&host);
            let (ran, failed) = tool(
                &first,
                "tilo_do",
                json!({"path":"notes/Todo/body#append-paragraph","args":{"text":"milk"}}),
            )
            .await;
            assert!(!failed, "{ran}");
            let later = agent(&host);
            let (history, failed) = history(&later).await;
            assert!(!failed, "{history}");
            assert!(
                history.contains("newest first (1 run)")
                    && history.contains("1 change in notes: append-paragraph"),
                "{history}"
            );
        });
    }

    phase another_agents_runs_are_not_theirs;
    {
        let host = KeepingRuns::of(Notes::new(true));
        run(async {
            let mine = agent(&host);
            let (ran, failed) = tool(
                &mine,
                "tilo_do",
                json!({"path":"notes/Todo/body#append-paragraph","args":{"text":"milk"}}),
            )
            .await;
            assert!(!failed, "{ran}");
            let other =
                McpServer::for_agent(host.clone(), "workspace".into(), "another-agent", "ada")
                    .with_agent_session("mcp:other");
            let (history, failed) = history(&other).await;
            assert!(!failed, "{history}");
            assert_eq!(
                history,
                "no run of this agent changed anything in this workspace yet"
            );
        });
    }
}

/// `history task` as an agent reads it: the words, and whether it failed.
async fn history<H: ActionProvider + Send + Sync + 'static>(
    server: &McpServer<H>,
) -> (String, bool) {
    tool(server, "tilo", json!({"program":"history task"})).await
}

/// The undo step each invocation the host was asked for joins, in order.
pub(super) fn gestures(host: &Notes) -> Vec<Option<tilo_actions::ActionId>> {
    host.invoked
        .lock()
        .unwrap()
        .iter()
        .map(|request| request.context.as_ref().and_then(|context| context.gesture))
        .collect()
}
