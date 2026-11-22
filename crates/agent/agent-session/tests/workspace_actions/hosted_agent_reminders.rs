//! A reminder a person's assistant set up, from its moment to the answer. The person's page is
//! closed; the moment comes; the server asks the assistant once, as the person, made from the
//! reminder and keyed by the step that handed it over: an ask of the agent package; the answer is
//! the ask's reply. Also where the server was down at the moment, and where it starts again
//! after. All through the server's own interfaces.
use super::*;
use crate::workspace_actions::hosted_agent_turns::hosted_initiative::writes_to_assistant;
use tilo_actions::remote::ReceiptStatus;
use tilo_actions::remote::workspace_events::{self, Ended, EventView, Events, Sort};
use tilo_server::workspace_bots::events::event_named;
use tilo_server::{
    ServerConfig as Config,
    agents::{AgentOptions, start_workspace_documents},
    daemon::{Server, ServerIdentity as Identity},
    workspace_bots::events::{Authority, WorkspaceEvents},
    workspaces::Workspaces,
};
use tilo_workspace_service::asks;

type Actions = tilo_agent_session::workspace_actions::WorkspaceActions;
/// The agents a server keeps working for its workspaces.
type Agents = ServerAgents;

/// A server that is up, as tilo.sh keeps one: its workspaces' documents served, and its people's
/// events kept going from its start.
struct Up {
    server: Server,
    agents: tilo_agent_session::AgentService,
    actions: Actions,
    events: WorkspaceEvents,
    workspaces: Workspaces,
}

impl Up {
    /// The server of `root`'s person on `device`, on the state under `state`. On the state a
    /// server before it kept, it is that server after a restart.
    async fn on(root: &SigningKey, device: &Device, state: &std::path::Path) -> Self {
        let cfg = Config {
            state_root: state.join("server"),
            packet_relay: Some((std::net::Ipv4Addr::LOCALHOST, 0).into()),
            ..Default::default()
        };
        let identity = Identity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: device.key.clone(),
            device_cert: device.cert.clone(),
            revocations: RevocationList::new(),
        };
        let (server, agents, roster) = start_workspace_documents(
            cfg,
            identity,
            AgentOptions {
                state_dir: state.join("agents"),
                catalogue: Some(Arc::new(crate::common::catalogue)),
                feedback_reviewer: None,
                shell: None,
                browsers: None,
            },
        )
        .await
        .unwrap();
        let workspaces = server.workspaces().unwrap().clone();
        Self {
            server,
            agents,
            actions: roster.actions,
            events: roster
                .events
                .expect("a server that keeps workspaces keeps their people's events"),
            workspaces,
        }
    }

    /// Stop the server, as when its process ends: nothing of it goes on.
    async fn down(self) {
        let Self {
            server,
            agents,
            actions,
            events,
            workspaces,
        } = self;
        agents.shutdown();
        drop((actions, events, workspaces));
        server.shutdown().await;
    }

    /// The agents this server keeps working, with `model`: their tools reach its people's
    /// events.
    fn agents(&self, model: Option<Model>) -> Agents {
        Agents::for_test(self.workspaces.clone(), self.actions.clone(), model)
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
    let page = Browser::open_at(&up.server, browser, device, &workspace).await;
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

/// A model that says what is due, whatever a turn asks it.
fn says_what_is_due() -> Model {
    waits(
        vec![says("It is time to call home.")],
        usize::MAX,
        &Arc::new(Mutex::new(Vec::new())),
        &Arc::new(Hold::default()),
    )
}

/// The asks of the agent package in Chat's record now, as the person's devices read them.
async fn asks_now(workspaces: &Workspaces, workspace: &str, person: &str) -> Vec<asks::Asked> {
    let packages = asks::chat_packages().unwrap();
    workspaces
        .with_tl_record(
            workspace,
            &packages,
            asks::Actor::Member(MemberId::person(person.to_owned())),
            |opened| asks::asks(opened),
        )
        .await
        .unwrap()
}

/// The event an ask is made from, where it is made from one.
fn event_of(asked: &asks::Asked) -> Option<&str> {
    asked.from.as_ref().and_then(event_named)
}

/// The ask an event of the person's made of their assistant, once Chat's record holds one.
async fn asked_by_a_cause(up: &Up, workspace: &str, person: &str) -> asks::Asked {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let all = asks_now(&up.workspaces, workspace, person).await;
        if let Some(asked) = all.into_iter().find(|asked| event_of(asked).is_some()) {
            return asked;
        }
        assert!(
            Instant::now() < deadline,
            "no reminder ever asked the assistant"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// The ask the event `event` made of the person's assistant, once Chat's record holds it.
async fn asked_from(up: &Up, workspace: &str, person: &str, event: &str) -> asks::Asked {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let all = asks_now(&up.workspaces, workspace, person).await;
        if let Some(asked) = all.into_iter().find(|asked| event_of(asked) == Some(event)) {
            return asked;
        }
        assert!(
            Instant::now() < deadline,
            "the event {event} never asked the assistant"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// What the assistant answered `ask` with, once its reply ended.
async fn answer_of(up: &Up, workspace: &str, person: &str, ask: &asks::ResourceRef) -> String {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let now = asks_now(&up.workspaces, workspace, person)
            .await
            .into_iter()
            .find(|asked| asked.at.same(ask))
            .expect("the ask is kept");
        if let Some(reply) = now.latest().filter(|reply| !reply.state.owed()) {
            return reply.body.clone();
        }
        assert!(Instant::now() < deadline, "the ask was never answered");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// `ask` as the record holds it now.
async fn now_of(up: &Up, workspace: &str, person: &str, ask: &asks::Asked) -> asks::Asked {
    asks_now(&up.workspaces, workspace, person)
        .await
        .into_iter()
        .find(|asked| asked.at.same(&ask.at))
        .expect("the ask is kept")
}

/// What the person's assistant sets up, as its own tool call sets one up: `name`, at a moment
/// `in_ms` from now. The reminder, and its moment.
fn set_up(up: &Up, workspace: &str, me: &Caller, name: &str, in_ms: i64) -> (EventView, i64) {
    let (when, at_ms) = in_a_moment(in_ms);
    let theirs = me.clone().delegated_to(SCOUT).named("Scout");
    let set_up = serde_json::json!({
        "name": name,
        "when": when,
        "instruction": "Say that it is time to call home.",
    });
    let reminder: EventView =
        serde_json::from_value(up.take(workspace, &theirs, workspace_events::SET_UP, &set_up))
            .expect("setting one up answers the event");
    assert_eq!(reminder.next_at_ms, Some(at_ms));
    (reminder, at_ms)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reminder_asks_its_persons_assistant_once_with_their_page_closed() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let device = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let (up, workspace, page) = team_on(&root, (&device, &browser), state.path()).await;
    page.peer.close();
    let person = hex::encode(root.verifying_key().to_bytes());
    let me = themselves(&root, &browser, &workspace);
    let (reminder, at_ms) = set_up(&up, &workspace, &me, "Call home", 2_000);
    assert_eq!(
        (reminder.name.as_str(), reminder.sort),
        ("Call home", Sort::Scheduled),
        "it is on a clock"
    );

    // The moment comes with nothing of the person's on. The server asks their assistant itself,
    // as them, made from the reminder, keyed by the step that handed it over.
    let ask = asked_by_a_cause(&up, &workspace, &person).await;
    assert!(
        tilo_clock::unix_now_ms_i64() >= at_ms,
        "it asks at its moment, not before"
    );
    assert_eq!(ask.agent, MemberId::agent(SCOUT));
    assert_eq!(
        ask.who,
        Some(asks::Actor::Member(MemberId::person(person.clone()))),
        "for the person the reminder is of"
    );
    assert_eq!(
        event_of(&ask),
        Some(reminder.id.as_str()),
        "made from the reminder"
    );
    assert!(!ask.mention.is_empty(), "keyed by its step");
    let agents = up.agents(Some(says_what_is_due()));
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "one turn is owed: {turns:?}");
    assert_eq!(
        (turns[0].person.as_str(), turns[0].cause.clone()),
        (person.as_str(), ask.at.id_string()),
    );
    assert!(
        turns[0]
            .request
            .contains("A reminder your person set is due now: \"Call home\".")
            && turns[0]
                .request
                .contains("Say that it is time to call home."),
        "the assistant is told what started the turn, then what to do: {}",
        turns[0].request
    );
    agents.answer(&workspace, &turns[0]).await.unwrap();

    // The answer is the ask's reply, and the reminder reads as invoked once.
    let answered = now_of(&up, &workspace, &person, &ask).await;
    assert_eq!(answered.state, asks::AskState::Done, "{answered:?}");
    let reply = answered.latest().expect("the ask has a reply");
    assert!(
        reply.body.contains("It is time to call home"),
        "{}",
        reply.body
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
        asks_now(&up.workspaces, &workspace, &person).await.len(),
        1,
        "nothing more was asked"
    );
    assert!(
        up.agents(None).turns(&workspace).await.unwrap().is_empty(),
        "and no turn is owed a second time"
    );
    up.down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reminder_due_while_its_server_was_down_asks_once_the_server_is_up_again() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let device = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let (up, workspace, page) = team_on(&root, (&device, &browser), state.path()).await;
    page.peer.close();
    let person = hex::encode(root.verifying_key().to_bytes());
    let me = themselves(&root, &browser, &workspace);
    let (reminder, at_ms) = set_up(&up, &workspace, &me, "Call home", 5_000);

    // The server stops before the moment, and is down when it comes.
    up.down().await;
    let left_ms = at_ms.saturating_sub(tilo_clock::unix_now_ms_i64());
    assert!(left_ms > 0, "the server was stopped before the moment");
    let past_it = u64::try_from(left_ms).unwrap_or(0).saturating_add(500);
    tokio::time::sleep(Duration::from_millis(past_it)).await;
    let up_again_ms = tilo_clock::unix_now_ms_i64();
    let up = Up::on(&root, &device, state.path()).await;

    // Up again, it asks what it owes, and the assistant answers it.
    let ask = asked_by_a_cause(&up, &workspace, &person).await;
    assert_eq!(event_of(&ask), Some(reminder.id.as_str()));
    let agents = up.agents(Some(says_what_is_due()));
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "one turn is owed: {turns:?}");
    agents.answer(&workspace, &turns[0]).await.unwrap();
    let answered = now_of(&up, &workspace, &person, &ask).await;
    let reply = answered.latest().expect("the ask has a reply");
    assert!(
        reply.body.contains("It is time to call home"),
        "{}",
        reply.body
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
    assert_eq!(asks_now(&up.workspaces, &workspace, &person).await.len(), 1);
    assert!(up.agents(None).turns(&workspace).await.unwrap().is_empty());
    assert_eq!(invocations(&up.events_of(&workspace, &me)[0]).0, 1);
    up.down().await;
}

/// The step that hands an instruction over is taken again, as an invocation tried again takes
/// it: it is the same ask, and the assistant owes one turn. Another step of the same reminder is
/// another ask, made from the same reminder.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_step_handed_over_again_asks_nothing_new() {
    use tilo_agent_session::bot::host_steps::{Instruction, StepCause};
    use tilo_server::workspace_bots::assistant::{self, Starter};
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let device = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let (up, workspace, page) = team_on(&root, (&device, &browser), state.path()).await;
    page.peer.close();
    let person = hex::encode(root.verifying_key().to_bytes());
    let steps = assistant::steps_starting_turns(
        up.workspaces.clone(),
        Starter {
            name: "Call home".into(),
            kind: assistant::REMINDER,
        },
    );
    let step = |invocation: InvocationId| Instruction {
        person: person.clone(),
        workspace: WorkspaceId::parse(&workspace).unwrap().to_string(),
        instruction: "Say that it is time to call home.".into(),
        allow: Vec::new(),
        permits: Vec::new(),
        note: serde_json::json!({}),
        cause: StepCause {
            installation: "call-home".into(),
            invocation,
            event: None,
        },
    };
    let hand = |instruction: Instruction| {
        let steps = Arc::clone(&steps);
        tokio::task::spawn_blocking(move || steps.instruct(instruction))
    };
    let first = InvocationId::new();
    for _ in 0..3 {
        assert_eq!(hand(step(first)).await.unwrap(), Ok(String::new()));
    }
    assert_eq!(
        asks_now(&up.workspaces, &workspace, &person).await.len(),
        1,
        "one step is one ask"
    );
    assert_eq!(up.agents(None).turns(&workspace).await.unwrap().len(), 1);
    assert_eq!(
        hand(step(InvocationId::new())).await.unwrap(),
        Ok(String::new())
    );
    let all = asks_now(&up.workspaces, &workspace, &person).await;
    assert_eq!(all.len(), 2, "another step is another ask: {all:?}");
    assert!(
        all.iter().all(|asked| event_of(asked) == Some("call-home")),
        "both made from the reminder: {all:?}"
    );
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
    person: String,
    /// The person's ask, as they read it answered before the kill.
    said: asks::Asked,
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
        page.peer.close();
        let person = hex::encode(root.verifying_key().to_bytes());
        writes_to_assistant(&up.workspaces, &workspace, &person, "What is due today?").await;
        let asked = Arc::new(Mutex::new(Vec::new()));
        let agents = up.agents(Some(waits(
            vec![looks(), says("Nothing is due today.")],
            usize::MAX,
            &asked,
            &Arc::new(Hold::default()),
        )));
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
        let said = asks_now(&up.workspaces, &workspace, &person)
            .await
            .pop()
            .expect("the ask is kept");
        assert_eq!(said.state, asks::AskState::Done, "{said:?}");
        let body = said
            .latest()
            .map(|reply| reply.body.clone())
            .unwrap_or_default();
        assert!(body.contains("Nothing is due today"), "{body}");
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
            person,
            said,
        }
    }
}

/// Whatever a kill cut off, the server that starts asks its model for that turn at most once:
/// what its model answers is taken, and nothing is owed for it again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn after_a_kill_a_start_asks_its_model_for_an_ended_turn_at_most_once() {
    let killed = Killed::as_an_answer_ended().await;
    let up = Up::on(&killed.root, &killed.device, killed.state.path()).await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = up.agents(Some(waits(
        vec![says("Nothing is due today.")],
        usize::MAX,
        &asked,
        &Arc::new(Hold::default()),
    )));
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

/// The answer a person read before the kill is ended after the next start too, with its words,
/// and nothing is owed that would say any of it a second time.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_killed_as_an_answer_ended_starts_with_the_answer_ended_and_owes_no_turn() {
    let killed = Killed::as_an_answer_ended().await;
    let up = Up::on(&killed.root, &killed.device, killed.state.path()).await;
    let all = asks_now(&up.workspaces, &killed.workspace, &killed.person).await;
    assert_eq!(all.len(), 1, "their ask, nothing else: {all:?}");
    let ask = &all[0];
    assert_eq!(
        ask.state,
        asks::AskState::Done,
        "the answer its person read is ended"
    );
    let body = |asked: &asks::Asked| asked.latest().map(|reply| reply.body.clone());
    assert_eq!(body(ask), body(&killed.said), "it says what it said");
    assert_eq!(ask.replies.len(), 1, "with one reply");
    let owed = up.agents(None).turns(&killed.workspace).await.unwrap();
    assert!(
        owed.is_empty(),
        "no turn is owed for an ask that was answered: {owed:?}"
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
