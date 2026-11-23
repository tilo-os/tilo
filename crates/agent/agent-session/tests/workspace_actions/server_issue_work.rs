//! An issue given to an agent on a workspace's server is worked on over many agent sessions: its
//! place is made before the first (its folder in the workspace's files, which an agent refused
//! Files does not get, and no note, since a server runs no tilolang app to keep one), a wait costs
//! nothing and ends at its moment or when the issue is told to go on, its budget stops it, and a
//! session that was lost is not started for ever. Nothing of it reads as done. No device of the
//! person is on for any of it, and a server that starts again reads the same from the workspace's
//! documents.
use super::*;
use serde_json::{Value, json};
use tasks::model::{StoppedWhy, TaskState};
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionMessage, workspace_events,
};
use tilo_server::workspaces::Workspaces;
use tilo_workspace_service::agents::{
    ServerAgents, Turn, WorkspaceActionProvider as ActionService, events::WorkspaceEvents,
    invoke_named,
};

/// A day, in milliseconds.
const DAY_MS: i64 = 86_400_000;

/// A workspace on a server with the agent Scout, and "Plan the launch" given to it by its person.
struct Given {
    root: SigningKey,
    browser: crate::support::Device,
    /// The person's page, open on the workspace: what their device writes goes through it.
    page: Browser,
    workspaces: Workspaces,
    actions: tilo_agent_session::workspace_actions::WorkspaceActions,
    workspace: String,
    /// The part of the tracker's issues that keeps the issue.
    document: String,
    issue: Id,
    /// The server.
    served: crate::harness::hosted::Hosted,
    /// Its state, kept for as long as the test lasts.
    _state: tempfile::TempDir,
}

async fn given() -> Given {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let served = crate::harness::hosted::start(
        &root,
        &server,
        state.path(),
        crate::harness::hosted::Options::offering(crate::common::catalogue),
    )
    .await;
    let actions = served.roster.actions.clone();
    let workspaces = served.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let page = Browser::open_at(&served, &browser, &server, &workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    make_agent(&page, &workspaces, &workspace, &person, None).await;
    let document = file_issue(
        &actions,
        &workspaces,
        &workspace,
        &root,
        (&server, &browser),
    )
    .await;
    let issue = tracker(&workspaces, &workspace, &document)
        .await
        .issues
        .iter()
        .next()
        .unwrap()
        .0
        .clone();
    Given {
        root,
        browser,
        page,
        workspaces,
        actions,
        workspace,
        document,
        issue,
        served,
        _state: state,
    }
}

impl Given {
    fn service(&self) -> ActionService {
        ActionService::new(
            self.actions.clone(),
            WorkspaceId::parse(&self.workspace).unwrap(),
        )
    }

    /// The person, at their page.
    fn person(&self) -> tilo_actions::remote::Caller {
        tilo_actions::remote::Caller::authenticated(
            self.root.verifying_key().to_bytes(),
            self.browser.cert.device_public_key,
        )
        .in_workspace(&self.workspace)
    }

    /// The person invokes Tasks' verb `action` on the issue.
    async fn on_issue(&self, action: &str, arguments: Value) -> Result<Value, String> {
        let mut arguments = arguments.as_object().cloned().unwrap_or_default();
        arguments.insert("issue".into(), json!(self.issue.as_str()));
        invoke_named(
            &self.service(),
            &self.person(),
            &DocumentRoute {
                app_id: TASKS.into(),
                cluster_id: self.workspace.clone(),
                document: self.document.clone(),
            },
            &tasks::model::issue_resource(&self.issue),
            action,
            arguments,
        )
        .await
    }

    async fn held(&self) -> tasks::model::Issue {
        tracker(&self.workspaces, &self.workspace, &self.document)
            .await
            .issues
            .get(&self.issue)
            .unwrap()
            .clone()
    }

    /// What the agent said in the issue's thread, oldest first.
    async fn said(&self) -> Vec<String> {
        self.held()
            .await
            .comments
            .values()
            .filter(|message| message.author == Some(ui::tilo_resource::MemberId::agent(SCOUT)))
            .map(|message| message.text.clone())
            .collect()
    }

    /// A server that just started: it holds nothing but the workspace's documents.
    async fn started(&self, model: Model) -> ServerAgents {
        self.served
            .approving(&self.workspace, &[SCOUT])
            .await
            .scripted(model)
    }

    /// The one turn the workspace owes the agent at `now_ms`.
    async fn owed(&self, agents: &ServerAgents, now_ms: i64) -> Turn {
        let turns = agents.turns_at(&self.workspace, now_ms).await.unwrap();
        assert_eq!(turns.len(), 1, "one turn is owed: {turns:?}");
        turns.into_iter().next().unwrap()
    }

    /// The one turn the workspace owes the agent at `now_ms`, answered.
    async fn answered(&self, agents: &ServerAgents, now_ms: i64) -> Turn {
        let turn = self.owed(agents, now_ms).await;
        // A turn's future is large: it is kept on the heap, so no test's own future carries it.
        let answer = Box::pin(agents.answer(&self.workspace, &turn));
        tokio::time::timeout(Duration::from_secs(60), answer)
            .await
            .expect("the turn ends")
            .unwrap();
        turn
    }

    /// Nothing is owed the agent at `now_ms`.
    async fn nothing_owed(&self, agents: &ServerAgents, now_ms: i64, why: &str) {
        let turns = agents.turns_at(&self.workspace, now_ms).await.unwrap();
        assert!(turns.is_empty(), "{why}: {turns:?}");
    }

    /// Its person narrows the agent, as their page does: its rules refuse it what Files keeps
    /// in the workspace, to read and to change.
    async fn refuses_the_agent_files(&self) {
        use tilo_action_policy::{ActionKind, Answer, KindSet, Rule, TreePath};
        let files = Rule::new(
            TreePath::workspace(&self.workspace).app(fs_protocol::tree::FILES_APP),
            KindSet::NONE
                .with(ActionKind::Read)
                .with(ActionKind::Undoable),
            Answer::Deny,
        );
        super::search::narrow_agent(
            &self.page,
            &self.workspaces,
            &self.workspace,
            &hex::encode(self.root.verifying_key().to_bytes()),
            vec![ui::tilo_workspace_agent::Bound::of(&files)],
        )
        .await;
    }

    /// What the folder `Tasks` of the workspace's files holds, by name, once it holds
    /// `at_least` entries or as it is after ten seconds: nothing while the workspace keeps no
    /// tree, or no such folder in it.
    async fn in_tasks_folder(&self, at_least: usize) -> Vec<String> {
        use fs_protocol::tree::{FILES_APP, ROOT_ID};
        let started = Instant::now();
        loop {
            let founded = self
                .workspaces
                .resource_document(
                    &self.workspace,
                    FILES_APP,
                    &fs_protocol::tree::document_id(),
                )
                .await
                .unwrap()
                .is_some();
            let mut names = Vec::new();
            if founded {
                let tree =
                    super::super::put_files::tree_kept(&self.workspaces, &self.workspace).await;
                let tasks = tree
                    .children(ROOT_ID)
                    .into_iter()
                    .find(|(_, node)| node.name.as_bytes() == b"Tasks")
                    .map(|(id, _)| id.to_owned());
                if let Some(tasks) = tasks {
                    names = tree
                        .children(&tasks)
                        .into_iter()
                        .map(|(_, node)| String::from_utf8_lossy(node.name.as_bytes()).into_owned())
                        .collect();
                }
            }
            if names.len() >= at_least || started.elapsed() > Duration::from_secs(10) {
                return names;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// The task waits for a reply until `until_ms`, as its person sets it.
    async fn waits_for_a_reply(&self, until_ms: i64) {
        self.on_issue(
            "wait",
            json!({"waits": [{"kind": "reply", "what": "a reply from the venue"}],
                   "until_ms": until_ms}),
        )
        .await
        .expect("the task waits");
    }
}

fn now_ms() -> i64 {
    tilo_clock::unix_now_ms_i64()
}

/// The words the model was given in its newest request.
fn told(asked: &Arc<Mutex<Vec<ModelRequest>>>) -> String {
    format!("{:?}", asked.lock().unwrap().last().unwrap().messages)
}

/// A model that takes its time over every answer: long enough for a server to die in the middle.
struct Slow {
    asked: Arc<Mutex<Vec<ModelRequest>>>,
}

impl tilo_agent_session::model::PlanTurns for Slow {
    fn turn(
        &mut self,
        request: &ModelRequest,
        _: &str,
        _: std::time::Instant,
        _: bool,
    ) -> Result<ModelResponse, ModelError> {
        self.asked.lock().unwrap().push(request.clone());
        std::thread::sleep(Duration::from_millis(1500));
        Ok(answer(
            vec![Content::Text {
                text: "Done at last.".into(),
            }],
            StopReason::EndTurn,
            1,
        ))
    }

    fn model(&self) -> &'static str {
        "slow"
    }
}

fn slow(asked: Arc<Mutex<Vec<ModelRequest>>>) -> Model {
    Arc::new(move || {
        Some(Box::new(Slow {
            asked: Arc::clone(&asked),
        }) as Box<dyn tilo_agent_session::model::PlanTurns>)
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_task_keeps_its_place_waits_two_days_and_a_restarted_server_goes_on() {
    let given = given().await;
    let asked = Arc::new(Mutex::new(Vec::new()));

    // The first session: the place is made before the model is asked, and the model is told where.
    let agents = given
        .started(looks_then_answers(1_000, Arc::clone(&asked)))
        .await;
    given.answered(&agents, now_ms()).await;
    let issue = given.held().await;
    let work = issue.work.clone().expect("the issue keeps its work");
    assert_eq!(work.folder, "Tasks/1 Plan the launch");
    let first = format!("{:?}", asked.lock().unwrap()[0].messages);
    assert!(
        work.note.is_empty(),
        "a server runs no tilolang app, so Notes makes no note: {:?}",
        work.note
    );
    assert!(
        issue.links.is_empty(),
        "with no note to link, the issue links nothing"
    );
    assert_eq!(
        (work.sessions, work.spent_micros, work.session_since_ms),
        (1, 2_000, None),
        "the session is counted and ended"
    );
    assert!(
        first.contains("It has no note") && first.contains("Notes could not make one"),
        "it is told plainly there is no note, and why: {first}"
    );
    assert_eq!(
        given.in_tasks_folder(1).await,
        ["1 Plan the launch"],
        "the folder the task names is made in the workspace's files before its first session"
    );
    assert!(first.contains("put files there"), "{first}");

    // The task waits two days for a reply.
    let until = now_ms() + 2 * DAY_MS;
    given.waits_for_a_reply(until).await;
    assert_eq!(
        tasks::model::task_state(&given.held().await),
        TaskState::Waiting {
            what: vec!["a reply from the venue".into()],
            until_ms: until
        }
    );

    // While it waits nothing is owed and no model is asked, also by a server that starts now.
    let calls = asked.lock().unwrap().len();
    let restarted = given
        .started(looks_then_answers(1_000, Arc::clone(&asked)))
        .await;
    for now in [now_ms(), until - 1] {
        given
            .nothing_owed(&restarted, now, "nothing is owed while it waits")
            .await;
    }
    assert_eq!(
        asked.lock().unwrap().len(),
        calls,
        "waiting costs no model call"
    );

    // The two days are over: the restarted server works on it again.
    let turn = given.answered(&restarted, until).await;
    assert_eq!(turn.cause, format!("waited-{until}"));
    let again = told(&asked);
    assert!(
        again.contains("It has no note"),
        "the later session is told there is no note, and why: {again}"
    );
    assert!(again.contains("The wait is over"), "{again}");
    let work = given.held().await.work.unwrap();
    assert_eq!(
        (work.sessions, work.until_ms),
        (2, None),
        "the session ended the wait"
    );
    assert_eq!(
        given.in_tasks_folder(1).await,
        ["1 Plan the launch"],
        "the later session found its folder there: nothing made a second one"
    );
    assert!(again.contains("put files there"), "{again}");
    given
        .nothing_owed(&restarted, until + DAY_MS, "answered once")
        .await;
}

/// An agent whose rules refuse it Files is given a task. The folder the task names is not made
/// for it: the server asks for it as the agent on its turn, with that turn's bound. The agent
/// starts its work all the same, and is told plainly that it has no folder.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_refused_files_starts_its_task_and_is_told_its_folder_was_not_made() {
    let given = given().await;
    given.refuses_the_agent_files().await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = given
        .started(looks_then_answers(1_000, Arc::clone(&asked)))
        .await;
    given.answered(&agents, now_ms()).await;

    let work = given.held().await.work.expect("the issue keeps its work");
    assert_eq!(
        (work.sessions, work.session_since_ms),
        (1, None),
        "its session began and ended"
    );
    assert_eq!(
        work.folder, "Tasks/1 Plan the launch",
        "the folder keeps its name on the issue"
    );
    let first = format!("{:?}", asked.lock().unwrap()[0].messages);
    assert!(
        work.note.is_empty() && first.contains("It has no note"),
        "it is told plainly there is no note, and why: {first}"
    );
    assert!(
        first.contains("It has no folder: `Tasks/1 Plan the launch` could not be made")
            && first.contains("may not invoke"),
        "it is told plainly that the folder was not made, and why: {first}"
    );
    assert!(
        !first.contains("put files there"),
        "it is not told to put files where there is no folder: {first}"
    );
    assert!(
        given.in_tasks_folder(0).await.is_empty(),
        "nothing made the folder its rules refuse it"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_task_resumes_on_the_reply_it_waited_for() {
    let given = given().await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = given
        .started(looks_then_answers(1_000, Arc::clone(&asked)))
        .await;
    given.answered(&agents, now_ms()).await;
    let until = now_ms() + 2 * DAY_MS;
    given.waits_for_a_reply(until).await;
    given
        .nothing_owed(&agents, now_ms(), "it waits for the reply")
        .await;

    // The reply comes: whatever saw it tells the task to go on.
    given
        .on_issue("resume", json!({"cause": "the venue replied"}))
        .await
        .unwrap();
    let turn = given.answered(&agents, now_ms()).await;
    assert!(turn.cause.starts_with("resumed-"), "{}", turn.cause);
    let again = told(&asked);
    assert!(again.contains("the venue replied"), "{again}");
    given
        .nothing_owed(
            &agents,
            until,
            "the deadline it no longer waits for wakes nothing",
        )
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_task_stops_at_its_budget_with_a_plain_line_and_never_reads_as_done() {
    let given = given().await;
    given
        .on_issue("set-budget", json!({"money_micros": 2_000_000}))
        .await
        .expect("its person sets the budget");
    let asked = Arc::new(Mutex::new(Vec::new()));
    // Each call costs $1.50: the second passes the budget of $2.00.
    let agents = given
        .started(looks_then_answers(1_500_000, Arc::clone(&asked)))
        .await;
    given.answered(&agents, now_ms()).await;
    let work = given.held().await.work.unwrap();
    assert_eq!(
        work.spent_micros, 3_000_000,
        "every call is counted on the task"
    );
    assert_eq!(work.used_up(), Some(StoppedWhy::Budget));

    // Its person writes again: nothing is worked on past the budget, and the task says why.
    given
        .on_issue("comment", json!({"text": "And the press list?"}))
        .await
        .unwrap();
    let calls = asked.lock().unwrap().len();
    given.answered(&agents, now_ms()).await;
    assert_eq!(
        asked.lock().unwrap().len(),
        calls,
        "no model call past the budget"
    );
    let issue = given.held().await;
    let read = tasks::model::task_state(&issue);
    assert!(
        matches!(&read, TaskState::Stopped { why: StoppedWhy::Budget, words }
            if words.contains("budget of $2.00 is spent")),
        "{read:?}"
    );
    assert!(
        !read.resolved(),
        "work that stopped at its budget is not done"
    );
    assert!(
        !issue.status.closed(),
        "nothing the server does closes it: {:?}",
        issue.status
    );
    let said = given.said().await;
    assert!(
        said.iter()
            .any(|said| said.contains("budget of $2.00 is spent")),
        "it says so in the thread: {said:?}"
    );

    // Told to go on, it is refused until its person raises the budget; then it works again.
    let refused = given
        .on_issue("resume", json!({"cause": "go on"}))
        .await
        .unwrap_err();
    assert!(refused.contains("raise it first"), "{refused}");
    given
        .on_issue("set-budget", json!({"money_micros": 9_000_000}))
        .await
        .unwrap();
    given
        .on_issue("resume", json!({"cause": "go on"}))
        .await
        .unwrap();
    given.answered(&agents, now_ms()).await;
    assert!(asked.lock().unwrap().len() > calls, "it works again");
    assert!(!matches!(
        tasks::model::task_state(&given.held().await),
        TaskState::Stopped { .. }
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_task_whose_sessions_were_lost_stops_and_says_so() {
    let given = given().await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = given.started(slow(Arc::clone(&asked))).await;
    // Three times a session begins and the server dies while its model answers: it never ends.
    for lost in 1..=3 {
        let turn = given.owed(&agents, now_ms()).await;
        let (dying, workspace) = (agents.clone(), given.workspace.clone());
        let answering = tokio::spawn(async move { dying.answer(&workspace, &turn).await });
        let deadline = tilo_test_env::hang_deadline();
        while asked.lock().unwrap().len() < lost {
            assert!(
                Instant::now() < deadline,
                "session {lost} never asked its model"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        answering.abort();
        let _ = answering.await;
        let work = given.held().await.work.unwrap();
        assert!(
            work.session_since_ms.is_some(),
            "session {lost} began and did not end"
        );
        assert_eq!(work.sessions, u64::try_from(lost).unwrap());
    }
    assert_eq!(given.held().await.work.unwrap().lost, 2);

    // The fourth time it is not started: it stops, says why, and does not read as done.
    given.answered(&agents, now_ms()).await;
    assert_eq!(
        asked.lock().unwrap().len(),
        3,
        "it is not started a fourth time"
    );
    let issue = given.held().await;
    let read = tasks::model::task_state(&issue);
    assert!(
        matches!(&read, TaskState::Stopped { why: StoppedWhy::LostWorker, words }
            if words.contains("interrupted")),
        "{read:?}"
    );
    assert!(
        !read.resolved() && !issue.status.closed(),
        "it does not read as done"
    );
    given
        .nothing_owed(
            &agents,
            now_ms(),
            "stopped, nothing starts it until its person says so",
        )
        .await;
}

/// One action on the person's automations, taken by the person on a device of theirs: what it
/// answered, or why it was not taken.
fn on_automations(
    automations: &WorkspaceEvents,
    given: &Given,
    action: &str,
    arguments: &Value,
) -> Result<Value, String> {
    let request = InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: workspace_events::target(action),
        argument: InvokeArgument::Json(arguments.as_object().cloned().unwrap_or_default()),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let answer = automations
        .serve(
            WorkspaceId::parse(&given.workspace).unwrap(),
            &given.person(),
            tilo_workspace_service::agents::events::Authority::Turn,
            RemoteActionMessage::Invoke(request),
        )
        .and_then(|answer| answer.wait_timeout(Duration::from_secs(5)).ok().flatten());
    match answer {
        Some(RemoteActionMessage::Receipt(receipt)) => match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
            other => Err(format!("{other:?}")),
        },
        _ => Err("an action is answered with its receipt".to_owned()),
    }
}

/// A task waits for one of its person's automations. When that automation comes, the server
/// tells the task to go on with what the automation says: no model is asked for the telling,
/// the task's agent is asked again from the issue, and an automation the task does not wait for
/// tells it nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_task_goes_on_when_the_automation_it_waits_for_comes() {
    let given = given().await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = given
        .started(looks_then_answers(1_000, Arc::clone(&asked)))
        .await;
    given.answered(&agents, now_ms()).await;
    let automations = WorkspaceEvents::start(
        given.workspaces.clone(),
        None,
        std::sync::Arc::new(tilo_server::event_runner::EventRunner),
    );
    let set_up = |name: &str, at_ms: i64, instruction: &str| {
        on_automations(
            &automations,
            &given,
            workspace_events::SET_UP,
            &json!({"name": name, "when": format!("once at {at_ms} ms"), "instruction": instruction}),
        )
        .expect("its person sets the automation up")["id"]
            .as_str()
            .expect("an automation has an id")
            .to_owned()
    };

    // The task waits for the venue's answer, two days at the most.
    let comes_ms = now_ms() + 2_500;
    let venue = set_up(
        "The venue's answer",
        comes_ms,
        "The venue confirmed the date.",
    );
    let until = now_ms() + 2 * DAY_MS;
    given
        .on_issue(
            "wait",
            json!({"waits": [{"kind": "change", "what": "the venue's answer", "event": venue}],
                   "until_ms": until}),
        )
        .await
        .expect("the task waits for the automation");
    given
        .nothing_owed(&agents, now_ms(), "it waits for the automation")
        .await;
    let calls = asked.lock().unwrap().len();

    // Another automation of its person comes first: the task does not wait for that one.
    set_up("Something else", now_ms() + 300, "The newsletter went out.");

    // The venue's automation comes: the server itself tells the task to go on.
    let deadline = tilo_test_env::hang_deadline();
    let resumed = loop {
        if let Some(resumed) = given.held().await.work.and_then(|work| work.resumed) {
            break resumed;
        }
        assert!(
            Instant::now() < deadline,
            "the automation never told the task to go on"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(resumed.cause, "The venue confirmed the date.");
    assert!(
        resumed.at_ms >= comes_ms,
        "only the automation it waits for told it, at its own moment"
    );
    assert_eq!(
        asked.lock().unwrap().len(),
        calls,
        "telling it to go on asks no model"
    );
    assert_eq!(given.held().await.work.unwrap().until_ms, None);

    // Its agent is asked again from the issue, with the automation's words.
    let turn = given.answered(&agents, now_ms()).await;
    assert!(turn.cause.starts_with("resumed-"), "{}", turn.cause);
    let again = told(&asked);
    assert!(again.contains("The venue confirmed the date."), "{again}");
    given
        .nothing_owed(
            &agents,
            until,
            "the deadline it no longer waits for wakes nothing",
        )
        .await;
}

/// A look reads what a workspace owes and enters a turn's place a moment later, when the turn
/// it read about may have ended. An issue's turn is begun once all the same: entered again
/// with what was read before its answer, it counts no second session, asks no model and says
/// nothing more in the issue's thread. The same holds for the turn of a wait that ended, read
/// at the wait's own moment.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_issues_turn_read_before_its_answer_is_not_begun_again() {
    let given = given().await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = given
        .started(looks_then_answers(1_000, Arc::clone(&asked)))
        .await;

    // What a look reads: the turn the issue is owed. It is answered, and its place is free.
    let read = given.answered(&agents, now_ms()).await;
    assert_eq!(asked.lock().unwrap().len(), 2, "it looked, then answered");
    assert_eq!(given.said().await.len(), 1);

    // The look that read before the answer enters the place only now.
    Box::pin(agents.answer(&given.workspace, &read))
        .await
        .unwrap();
    assert_eq!(
        asked.lock().unwrap().len(),
        2,
        "the issue is asked of the model once"
    );
    assert_eq!(given.said().await.len(), 1, "one answer in its thread");
    assert_eq!(
        given.held().await.work.unwrap().sessions,
        1,
        "and one session counted on it"
    );

    // It waits two days. The turn of that wait's end is read at its moment, and answered.
    let until = now_ms() + 2 * DAY_MS;
    given.waits_for_a_reply(until).await;
    let waited = given.answered(&agents, until).await;
    assert_eq!(waited.cause, format!("waited-{until}"));
    assert_eq!(asked.lock().unwrap().len(), 4, "the later session");

    // Entered again with that reading, it is not begun a second time either.
    Box::pin(agents.answer(&given.workspace, &waited))
        .await
        .unwrap();
    assert_eq!(
        asked.lock().unwrap().len(),
        4,
        "the wait's end is asked of the model once"
    );
    assert_eq!(given.held().await.work.unwrap().sessions, 2);
    assert_eq!(given.said().await.len(), 2);
    given
        .nothing_owed(&agents, until + DAY_MS, "answered once")
        .await;
}

/// A workspace nobody opened Tasks in keeps no tracker, and its server offers nothing of Tasks:
/// from a device that shows no Tasks a person could give their agent no issue. A server that
/// keeps an agent at work for the workspace founds the tracker at its look, as a member's first
/// window of Tasks does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_with_an_agent_founds_the_tracker_of_a_workspace_nobody_opened_tasks_in() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let served = crate::harness::hosted::start(
        &root,
        &server,
        state.path(),
        crate::harness::hosted::Options::offering(crate::common::catalogue),
    )
    .await;
    let actions = served.roster.actions.clone();
    let workspaces = served.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let page = Browser::open_at(&served, &browser, &server, &workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    make_agent(&page, &workspaces, &workspace, &person, None).await;
    // The person gives the agent an issue through the server's action service, as a device of
    // theirs that shows no Tasks does.
    let give = || async {
        let catalogue = crate::common::catalogue();
        let route = DocumentRoute {
            app_id: TASKS.into(),
            cluster_id: workspace.clone(),
            document: tilo_workspace_service::agents::workspace_resource(&catalogue, TASKS),
        };
        let person = tilo_actions::remote::Caller::authenticated(
            root.verifying_key().to_bytes(),
            browser.cert.device_public_key,
        )
        .in_workspace(&workspace);
        let arguments = json!({"title": "Plan the launch", "assignee": format!("agent:{SCOUT}")});
        invoke_named(
            &ActionService::new(actions.clone(), WorkspaceId::parse(&workspace).unwrap()),
            &person,
            &route,
            &ui::resource::chain_resource_id(&tasks::issue_list::chain()),
            "create",
            arguments.as_object().unwrap().clone(),
        )
        .await
    };
    // Nobody opened Tasks here: until the server looked, nothing of Tasks is offered.
    let refused = give().await;
    assert!(refused.is_err(), "nothing keeps a tracker yet: {refused:?}");

    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = served
        .approving(&workspace, &[SCOUT])
        .await
        .scripted(slow(asked));
    let owed = agents.turns_at(&workspace, now_ms()).await.unwrap();
    assert!(owed.is_empty(), "no issue is given yet: {owed:?}");

    // The look founded the tracker: the same ask is taken now, and the issue owes its agent a turn.
    give()
        .await
        .expect("the person gives their agent an issue where nobody opened Tasks");
    let owed = agents.turns_at(&workspace, now_ms()).await.unwrap();
    assert_eq!(owed.len(), 1, "the issue owes its agent a turn: {owed:?}");
    assert!(
        matches!(
            owed[0].ask,
            tilo_workspace_service::agents::Ask::Issue { .. }
        ),
        "{:?}",
        owed[0].ask
    );
}
