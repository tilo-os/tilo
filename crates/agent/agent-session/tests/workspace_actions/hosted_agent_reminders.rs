//! A reminder a person's assistant set up itself, from the asking to the answer. The person
//! writes to their assistant and closes their page; the assistant's own tool call sets the
//! reminder up on the server; the moment comes; the server asks the assistant through Chat's
//! `ask-agent`, once; the answer is in their conversation. Also where the server was down at
//! the moment, and where it starts again after. All through the server's own interfaces.
use super::*;
use tilo_actions::remote::ReceiptStatus;
use tilo_actions::remote::workspace_events::{self, Ended, EventView, Events, Sort};
use tilo_server::workspaces::Workspaces;
use tilo_workspace_service::agents::events::{Authority, WorkspaceEvents};

type Actions = tilo_agent_session::workspace_actions::WorkspaceActions;
/// The agents a server keeps working for its workspaces.
type Agents = ServerAgents;

/// A server that is up, as tilo.sh keeps one: its workspaces' documents served, and its people's
/// events kept going from its start.
struct Up {
    hosted: crate::harness::hosted::Hosted,
    actions: Actions,
    events: WorkspaceEvents,
    workspaces: Workspaces,
}

impl Up {
    /// The server of `root`'s person on `device`, on the state under `state`. On the state a
    /// server before it kept, it is that server after a restart.
    async fn on(root: &SigningKey, device: &Device, state: &std::path::Path) -> Self {
        let hosted = crate::harness::hosted::start(
            root,
            device,
            state,
            crate::harness::hosted::Options::offering(crate::common::catalogue),
        )
        .await;
        Self {
            actions: hosted.roster.actions.clone(),
            events: hosted
                .roster
                .events
                .clone()
                .expect("a server that keeps workspaces keeps their people's events"),
            workspaces: hosted.workspaces().unwrap().clone(),
            hosted,
        }
    }

    /// Stop the server, as when its process ends: nothing of it goes on.
    async fn down(self) {
        let Self {
            hosted,
            actions,
            events,
            workspaces,
        } = self;
        hosted.agents.shutdown();
        drop((actions, events, workspaces));
        hosted.server.shutdown().await;
    }

    /// The agents this server keeps working in `workspace`, with `model`: their tools reach its
    /// people's events.
    async fn agents(&self, workspace: &str, model: Option<Model>) -> Agents {
        self.hosted
            .approving(workspace, &[SCOUT])
            .await
            .scripted(model)
            .with_events(Some(self.events.clone()))
    }

    /// One action on the events of the person `caller` is or acts for, as the server answers it.
    fn take(
        &self,
        workspace: &str,
        caller: &Caller,
        action: &str,
        arguments: &serde_json::Value,
    ) -> serde_json::Value {
        let request = InvokeRequest {
            invocation: InvocationId::new(),
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
        let workspace = WorkspaceId::parse(workspace).unwrap();
        let receipt = self
            .events
            .invoking(workspace, caller, Authority::Turn, request)
            .wait_timeout(std::time::Duration::from_secs(30))
            .map_err(|_| "the server did not answer an action on its events")
            .unwrap()
            .expect("the server abandoned an action on its events");
        match receipt.status {
            ReceiptStatus::Completed { output } => output.value.unwrap_or_default(),
            other => panic!("{action} on the workspace's events: {other:?}"),
        }
    }

    /// The events of the person `me` is, as a device of theirs reads them from the server.
    fn events_of(&self, workspace: &str, me: &Caller) -> Vec<EventView> {
        let listed = self.take(
            workspace,
            me,
            workspace_events::LIST,
            &serde_json::json!({}),
        );
        serde_json::from_value::<Events>(listed)
            .expect("the list reads as events")
            .events
    }

    /// The person's one event, once the server kept how an invocation of it ended.
    async fn ran(&self, workspace: &str, me: &Caller) -> EventView {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let theirs = self.events_of(workspace, me);
            if let [event] = theirs.as_slice()
                && invocations(event).1.is_some()
            {
                return event.clone();
            }
            assert!(
                Instant::now() < deadline,
                "the server never kept how the reminder's invocation ended: {theirs:?}"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// The message a cause asked the assistant with, once the conversation holds one.
    async fn asked_by_a_cause(&self, workspace: &str) -> (Id, chat::chat::Message) {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let part = conversation_part(&self.workspaces, workspace).await;
            if let Some((id, held)) = part.messages.iter().find(|(_, held)| held.cause.is_some()) {
                return (id.clone(), held.clone());
            }
            assert!(
                Instant::now() < deadline,
                "no reminder ever asked the assistant"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

/// How often `event` was invoked, and how and when its last invocation ended.
fn invocations(event: &EventView) -> (u64, Option<(Ended, i64)>) {
    let last = event.last_run.as_ref();
    (event.runs, last.map(|last| (last.ended, last.at_ms)))
}

/// A workspace on a new server under `state`, with one person in its chat and their assistant
/// made to work on the server. The person's page is open: they close it themselves.
async fn team_on(
    root: &SigningKey,
    (device, browser): (&Device, &Device),
    state: &std::path::Path,
) -> (Up, String, Browser) {
    let up = Up::on(root, device, state).await;
    let workspace = up.workspaces.create("Team").await.unwrap().workspace;
    let page = Browser::open_at(&up.hosted, browser, device, &workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    make_agent(&page, &up.workspaces, &workspace, &person, None).await;
    let catalogue = crate::common::catalogue();
    let governor = up.workspaces.governor().clone();
    up.workspaces
        .prepare_application(&workspace, &governor, &catalogue, CHAT, "")
        .await
        .unwrap();
    (up, workspace, page)
}

/// A moment `in_ms` from now, as an event's `when` says one and in ms since the epoch.
fn in_a_moment(in_ms: i64) -> (String, i64) {
    let at_ms = tilo_clock::unix_now_ms_i64().saturating_add(in_ms);
    (format!("once at {at_ms} ms"), at_ms)
}

/// The assistant's model, written out. Asked to remind its person, it sets the reminder up with
/// the server's own action, through the tools every turn has, for a moment `in_ms` after it is
/// asked, and then says so. The moment it chose is kept in `at_ms`.
struct Reminds {
    in_ms: i64,
    at_ms: Arc<Mutex<Option<i64>>>,
    asked: Arc<Mutex<Vec<ModelRequest>>>,
}

impl tilo_agent_session::model::PlanTurns for Reminds {
    fn turn(
        &mut self,
        request: &ModelRequest,
        _: &str,
        _: std::time::Instant,
        _: bool,
    ) -> Result<ModelResponse, ModelError> {
        self.asked.lock().unwrap().push(request.clone());
        let mut chosen = self.at_ms.lock().unwrap();
        if chosen.is_some() {
            return Ok(says("I will remind you to call home."));
        }
        let (when, at_ms) = in_a_moment(self.in_ms);
        *chosen = Some(at_ms);
        Ok(answer(
            vec![Content::ToolCall(ToolCall {
                id: "call-1".into(),
                name: "tilo_do".into(),
                input: serde_json::json!({
                    "path": "events#set-up",
                    "args": {
                        "name": "Call home",
                        "when": when,
                        "instruction": "Say that it is time to call home.",
                    },
                }),
                extra_content: None,
            })],
            StopReason::ToolCalls,
            1_000,
        ))
    }

    fn model(&self) -> &'static str {
        "reminds"
    }
}

/// A model that says what is due, whatever a turn asks it.
fn says_what_is_due() -> Model {
    waits(
        vec![says("It is time to call home.")],
        usize::MAX,
        &Arc::new(Mutex::new(Vec::new())),
        &Arc::new(Hold::default()),
    )
}

/// The person asks their assistant on `up` for a reminder and closes their page at once. The
/// assistant's turn, with nothing of the person's on, sets the reminder up for a moment `in_ms`
/// after it is asked. The reminder as its person then reads it, and its moment.
async fn reminded(
    up: &Up,
    workspace: &str,
    (root, browser): (&SigningKey, &Device),
    page: &Browser,
    in_ms: i64,
) -> (EventView, i64) {
    let wish = write_to_agent(
        &up.actions,
        &up.workspaces,
        workspace,
        (root, browser),
        "Remind me in a moment to call home.",
    )
    .await;
    page.peer.close();
    let at_ms = Arc::new(Mutex::new(None));
    let asked = Arc::new(Mutex::new(Vec::new()));
    let model: Model = {
        let (at_ms, asked) = (Arc::clone(&at_ms), Arc::clone(&asked));
        Arc::new(move || {
            Some(Box::new(Reminds {
                in_ms,
                at_ms: Arc::clone(&at_ms),
                asked: Arc::clone(&asked),
            })
                as Box<dyn tilo_agent_session::model::PlanTurns>)
        })
    };
    let agents = up.agents(workspace, Some(model)).await;
    let turns = agents.turns(workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    agents.answer(workspace, &turns[0]).await.unwrap();
    let said = ended(&up.workspaces, workspace, &wish).await;
    assert!(said.text.contains("I will remind you"), "{}", said.text);
    let theirs = up.events_of(workspace, &themselves(root, browser, workspace));
    assert_eq!(
        theirs.len(),
        1,
        "the assistant's own tool call sets the reminder up; its model was last asked {:?}",
        asked.lock().unwrap().last()
    );
    let at_ms = at_ms.lock().unwrap().expect("the assistant chose a moment");
    (theirs[0].clone(), at_ms)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reminder_the_assistant_set_up_itself_starts_its_turn_with_its_persons_page_closed() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let device = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let (up, workspace, page) = team_on(&root, (&device, &browser), state.path()).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let me = themselves(&root, &browser, &workspace);

    // The person asks and closes their page. The assistant, on the server, sets the reminder up.
    let (reminder, at_ms) = reminded(&up, &workspace, (&root, &browser), &page, 2_000).await;
    assert_eq!(
        (reminder.name.as_str(), reminder.sort),
        ("Call home", Sort::Scheduled),
        "it is on a clock"
    );

    // The moment comes with nothing of the person's on. The server asks their assistant itself,
    // as their reminder and for them.
    let (ask, message) = up.asked_by_a_cause(&workspace).await;
    assert!(
        tilo_clock::unix_now_ms_i64() >= at_ms,
        "it asks at its moment, not before"
    );
    let cause = message.cause.as_ref().expect("it carries its cause");
    assert_eq!(
        (cause.kind.0.as_str(), cause.label.as_str()),
        ("reminder", "Call home")
    );
    assert_eq!(cause.person, MemberId::person(person.clone()));
    assert_eq!(
        message.author,
        MemberId::agent(reminder.id.as_str()),
        "the reminder asked, never the person"
    );
    let agents = up.agents(&workspace, Some(says_what_is_due())).await;
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "one turn is owed: {turns:?}");
    assert_eq!(
        (turns[0].person.as_str(), turns[0].cause.clone()),
        (person.as_str(), ask.to_string()),
        "for the person the reminder is of"
    );
    assert!(
        turns[0]
            .request
            .starts_with("A reminder your person set is due now: \"Call home\".")
            && turns[0]
                .request
                .contains("Say that it is time to call home."),
        "the assistant is told what started the turn, then what to do: {}",
        turns[0].request
    );
    agents.answer(&workspace, &turns[0]).await.unwrap();

    // The answer is in their conversation, and the reminder reads as invoked once.
    let reply = ended(&up.workspaces, &workspace, &ask).await;
    assert!(
        reply.text.contains("It is time to call home"),
        "{}",
        reply.text
    );
    assert_eq!(
        conversation_part(&up.workspaces, &workspace)
            .await
            .messages
            .len(),
        4,
        "what the person asked and its answer, the reminder and its answer"
    );
    assert!(
        agents.turns(&workspace).await.unwrap().is_empty(),
        "answered once"
    );
    let kept = up.ran(&workspace, &me).await;
    let (times, last) = invocations(&kept);
    assert_eq!((times, kept.next_at_ms), (1, None));
    assert!(
        last.is_some_and(|(ended, ended_ms)| ended == Ended::Ran && ended_ms >= at_ms),
        "it was invoked at its moment, not before: {kept:?}"
    );

    // The server starts again: it keeps the reminder as invoked once, and asks nothing a second
    // time.
    drop(agents);
    up.down().await;
    let up = Up::on(&root, &device, state.path()).await;
    let after = up.events_of(&workspace, &me);
    assert_eq!(after.len(), 1, "{after:?}");
    assert_eq!(
        (after[0].id.as_str(), invocations(&after[0]).0),
        (kept.id.as_str(), 1)
    );
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert_eq!(
        conversation_part(&up.workspaces, &workspace)
            .await
            .messages
            .len(),
        4,
        "nothing more was asked"
    );
    assert!(
        up.agents(&workspace, None)
            .await
            .turns(&workspace)
            .await
            .unwrap()
            .is_empty(),
        "and no turn is owed a second time"
    );
    up.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reminder_due_while_its_server_was_down_starts_its_turn_once_the_server_is_up_again() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let device = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let (up, workspace, page) = team_on(&root, (&device, &browser), state.path()).await;
    page.peer.close();
    let me = themselves(&root, &browser, &workspace);

    // Their assistant sets a reminder up, through the entry its turn's tools reach (the test
    // above goes through a turn).
    let (when, at_ms) = in_a_moment(5_000);
    let theirs = super::their_agent(&me, SCOUT).named("Scout");
    let set_up = serde_json::json!({
        "name": "Call home",
        "when": when,
        "instruction": "Say that it is time to call home.",
    });
    let reminder: EventView =
        serde_json::from_value(up.take(&workspace, &theirs, workspace_events::SET_UP, &set_up))
            .expect("setting one up answers the event");
    assert_eq!(reminder.next_at_ms, Some(at_ms));

    // The server stops before the moment, and is down when it comes.
    up.down().await;
    let left_ms = at_ms.saturating_sub(tilo_clock::unix_now_ms_i64());
    assert!(left_ms > 0, "the server was stopped before the moment");
    let past_it = u64::try_from(left_ms).unwrap_or(0).saturating_add(500);
    tokio::time::sleep(Duration::from_millis(past_it)).await;
    let up_again_ms = tilo_clock::unix_now_ms_i64();
    let up = Up::on(&root, &device, state.path()).await;

    // Up again, it asks what it owes, and the assistant answers in the conversation.
    let (ask, message) = up.asked_by_a_cause(&workspace).await;
    assert_eq!(
        message.cause.as_ref().map(|cause| cause.label.as_str()),
        Some("Call home")
    );
    let agents = up.agents(&workspace, Some(says_what_is_due())).await;
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "one turn is owed: {turns:?}");
    agents.answer(&workspace, &turns[0]).await.unwrap();
    let reply = ended(&up.workspaces, &workspace, &ask).await;
    assert!(
        reply.text.contains("It is time to call home"),
        "{}",
        reply.text
    );
    let kept = up.ran(&workspace, &me).await;
    let (times, last) = invocations(&kept);
    assert_eq!((kept.id.as_str(), times), (reminder.id.as_str(), 1));
    assert!(
        last.is_some_and(|(ended, ended_ms)| ended == Ended::Ran && ended_ms >= up_again_ms),
        "it was invoked once the server was up again, not while it was down: {kept:?}"
    );

    // Started once more, the server asks nothing a second time.
    drop(agents);
    up.down().await;
    let up = Up::on(&root, &device, state.path()).await;
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert_eq!(
        conversation_part(&up.workspaces, &workspace)
            .await
            .messages
            .len(),
        2,
        "the reminder and its answer, nothing else"
    );
    assert!(
        up.agents(&workspace, None)
            .await
            .turns(&workspace)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(invocations(&up.events_of(&workspace, &me)[0]).0, 1);
    up.down().await;
}

/// What a server keeps under `dir` at this moment: every file with its length and when it was
/// last written.
fn kept_under(
    dir: &std::path::Path,
    files: &mut std::collections::BTreeMap<
        std::path::PathBuf,
        (u64, Option<std::time::SystemTime>),
    >,
) {
    // A folder or a file removed since it was listed is not part of this state.
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        match entry.metadata() {
            Ok(meta) if meta.is_dir() => kept_under(&entry.path(), files),
            Ok(meta) => {
                files.insert(entry.path(), (meta.len(), meta.modified().ok()));
            }
            Err(_) => {}
        }
    }
}

/// Copy what `from` holds into `to`.
fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).into_iter().flatten().flatten() {
        let target = to.join(entry.file_name());
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            copy_dir(&entry.path(), &target);
        } else {
            let _ = std::fs::copy(entry.path(), &target);
        }
    }
}

/// Copy what a server keeps under `from` into `to`, as a kill of its process at this moment
/// leaves it on disk. Whether the copy is one moment's state: nothing under `from` was written,
/// added or removed while it was taken.
fn killed_into(from: &std::path::Path, to: &std::path::Path) -> bool {
    let mut before = std::collections::BTreeMap::new();
    kept_under(from, &mut before);
    copy_dir(from, to);
    let mut after = std::collections::BTreeMap::new();
    kept_under(from, &mut after);
    before == after
}

/// What a kill of a server's process left, the moment its assistant's turn was over: no stop of
/// the server's was made, and the next start has what its disk held then.
struct Killed {
    root: SigningKey,
    device: Device,
    /// What the server's disk held at the kill.
    state: tempfile::TempDir,
    workspace: String,
    /// The person's message, and the answer to it as they read it before the kill.
    wish: Id,
    said: chat::chat::Message,
}

impl Killed {
    /// A person asks their assistant, it looks through the workspace and answers, and its turn
    /// is over: the process is killed there.
    async fn as_an_answer_ended() -> Self {
        let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
        let device = enrol(&root, "tilo-sh", DeviceCapabilities::member());
        let browser = enrol(&root, "browser", DeviceCapabilities::member());
        let state = crate::support::scratch_dir();
        let (up, workspace, page) = team_on(&root, (&device, &browser), state.path()).await;
        let wish = write_to_agent(
            &up.actions,
            &up.workspaces,
            &workspace,
            (&root, &browser),
            "What is due today?",
        )
        .await;
        page.peer.close();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let agents = up
            .agents(
                &workspace,
                Some(waits(
                    vec![looks(), says("Nothing is due today.")],
                    usize::MAX,
                    &asked,
                    &Arc::new(Hold::default()),
                )),
            )
            .await;
        let turns = agents.turns(&workspace).await.unwrap();
        assert_eq!(turns.len(), 1, "{turns:?}");
        agents.answer(&workspace, &turns[0]).await.unwrap();

        // The kill, here: what the disk holds at this moment is all the next start has.
        let deadline = tilo_test_env::hang_deadline();
        let killed = loop {
            let killed = crate::support::scratch_dir();
            if killed_into(state.path(), killed.path()) {
                break killed;
            }
            assert!(
                Instant::now() < deadline,
                "what the server keeps never stood still for a copy"
            );
        };
        let said = ended(&up.workspaces, &workspace, &wish).await;
        assert!(said.text.contains("Nothing is due today"), "{}", said.text);
        assert!(
            agents.turns(&workspace).await.unwrap().is_empty(),
            "the turn was over before the kill"
        );
        drop(agents);
        up.down().await;
        Self {
            root,
            device,
            state: killed,
            workspace,
            wish,
            said,
        }
    }
}

/// Whatever a kill cut off, the server that starts asks its model for that turn at most once:
/// what its model answers is taken, and nothing is owed for it again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn after_a_kill_a_start_asks_its_model_for_an_ended_turn_at_most_once() {
    let killed = Box::pin(Killed::as_an_answer_ended()).await;
    let up = Up::on(&killed.root, &killed.device, killed.state.path()).await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = up
        .agents(
            &killed.workspace,
            Some(waits(
                vec![says("Nothing is due today.")],
                usize::MAX,
                &asked,
                &Arc::new(Hold::default()),
            )),
        )
        .await;
    // What the server's own look does, three times over: it starts every turn that is owed.
    for _ in 0..3 {
        for turn in agents.turns(&killed.workspace).await.unwrap() {
            agents.answer(&killed.workspace, &turn).await.unwrap();
        }
    }
    let times = asked.lock().unwrap().len();
    assert!(
        times <= 1,
        "its model was asked {times} times for a turn that was over before the kill"
    );
    drop(agents);
    up.down().await;
}

/// The answer a person read before the kill is ended after the next start too, with its words
/// and its steps, and nothing is owed that would say or do any of it a second time.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_killed_as_an_answer_ended_starts_with_the_answer_ended_and_owes_no_turn() {
    let killed = Box::pin(Killed::as_an_answer_ended()).await;
    let up = Up::on(&killed.root, &killed.device, killed.state.path()).await;
    let part = conversation_part(&up.workspaces, &killed.workspace).await;
    let reply = part
        .messages
        .get(&chat::chat::reply_id(&killed.wish, &MemberId::agent(SCOUT)));
    assert!(
        reply.is_some_and(|reply| !chat::summons::unanswered(Some(reply))),
        "the answer its person read is ended after the start: {:?}",
        reply.map(|reply| reply.work.clone())
    );
    let reply = reply.expect("the answer is there");
    assert_eq!(reply.text, killed.said.text, "it says what it said");
    let steps =
        |reply: &chat::chat::Message| reply.work.as_ref().map_or(0, |work| work.trace.len());
    assert_eq!(steps(reply), steps(&killed.said), "with the steps it took");
    assert_eq!(
        part.messages.len(),
        2,
        "their message and its answer, nothing else"
    );
    let owed = up
        .agents(&killed.workspace, None)
        .await
        .turns(&killed.workspace)
        .await
        .unwrap();
    assert!(
        owed.is_empty(),
        "no turn is owed for a message that was answered: {owed:?}"
    );
    up.down().await;
}

/// How the result of an event is told to its person, where the event says.
#[path = "hosted_event_told_as.rs"]
mod told_as;

/// What a turn that a reminder started tells its person when it ends.
#[path = "hosted_agent_tells.rs"]
mod tells;

/// A rundown before a meeting a calendar holds, from the asking to the telling.
#[path = "meeting_rundown.rs"]
mod meeting_rundown;
