//! A `Notes` workspace whose host also keeps its runs, as every workspace
//! host does: each change is a run of the member that made it, a listing
//! answers the runs of the member it is asked for, and `history task` reads
//! them beside the steps of this task.
use super::*;
use tilo_actions::remote::workspace_invocations::{
    Invocation, InvocationBy, InvocationChange, InvocationStatus, Invocations, InvocationsQuery,
    is_listing,
};

/// [`Notes`] with the runs a workspace host keeps of what happened in it.
#[derive(Clone)]
pub(super) struct KeepingRuns {
    notes: Notes,
    /// The runs the workspace keeps, newest last.
    runs: Arc<Mutex<Vec<Invocation>>>,
    /// The member each listing of runs asked for, oldest first.
    asked: Arc<Mutex<Vec<Option<String>>>>,
}

impl KeepingRuns {
    /// `notes` as a workspace whose host keeps the runs its changes make.
    pub(super) fn of(notes: Notes) -> Self {
        Self {
            notes,
            runs: Arc::default(),
            asked: Arc::default(),
        }
    }

    /// The member each listing of runs asked for, oldest first.
    fn runs_asked(&self) -> Vec<Option<String>> {
        self.asked.lock().unwrap().clone()
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

    /// The workspace's runs, newest first, as its host lists them: only the
    /// member's the query asks for, and one page of them.
    fn listing(&self, request: &InvokeRequest) -> InvocationReceipt {
        let query = match &request.argument {
            InvokeArgument::Json(fields) => serde_json::from_value::<InvocationsQuery>(
                serde_json::Value::Object(fields.clone()),
            )
            .unwrap_or_default(),
            _ => InvocationsQuery::default(),
        };
        self.asked.lock().unwrap().push(query.by.clone());
        let mut runs = self.runs.lock().unwrap().clone();
        if let Some(by) = &query.by {
            runs.retain(|run| run.by.id.as_ref() == Some(by));
        }
        runs.sort_by_key(|run| std::cmp::Reverse(run.started_ms));
        let total = runs.len();
        runs.truncate(query.page());
        let listed = Invocations {
            items: runs,
            total,
            waiting: 0,
            pending: false,
            next_cursor: None,
        };
        let value = serde_json::to_value(&listed).ok();
        InvocationState::new(
            request.invocation,
            InvocationUpdate::Succeeded {
                invocation: request.invocation,
                output: tilo_actions::ActionOutput {
                    value,
                    ..tilo_actions::ActionOutput::default()
                },
            },
        )
        .receipt()
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

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if is_listing(&request.target) {
            return Pending::ready(Ok(self.listing(&request)));
        }
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
            // The workspace is asked for this agent's runs alone.
            assert_eq!(host.runs_asked(), vec![Some("claude-code".to_owned())]);
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
