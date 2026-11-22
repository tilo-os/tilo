//! Sub-agents on the server, with no device of the person running anything: the person's own
//! agent starts one, which is asked its job through the agent package in its starter's name and
//! answers it as its own reply; a note asks it again, a stop takes back what was asked before
//! it, and when work its starter gave it ends, its starter is asked with the news and reads the
//! reply. A cause asks once however often a look finds it.
use super::*;
use tilo_server::workspaces::Workspaces;
use tilo_workspace_service::asks::{self, AskState};
use tilo_workspace_service::tl_records::runtime::Value;
use ui::tilo_resource::{Invoke, NonBlank, Replica};
use ui::tilo_workspace_agent::{Agent, NewSubAgent, Stands, workspace_agents};

/// A coding agent: a sub-agent whose work a coding program does in the workspace's machine.
#[path = "hosted_coding_agents.rs"]
mod hosted_coding_agents;

/// A model whose answer to each call is `play`'s. Every call is kept, in the order asked.
struct Plays {
    play: Arc<dyn Fn(&ModelRequest) -> ModelResponse + Send + Sync>,
    asked: Arc<Mutex<Vec<ModelRequest>>>,
}

impl tilo_agent_session::model::PlanTurns for Plays {
    fn turn(
        &mut self,
        request: &ModelRequest,
        _: &str,
        _: std::time::Instant,
        _: bool,
    ) -> Result<ModelResponse, ModelError> {
        self.asked.lock().unwrap().push(request.clone());
        Ok((self.play)(request))
    }

    fn model(&self) -> &'static str {
        "plays"
    }
}

fn plays(
    asked: &Arc<Mutex<Vec<ModelRequest>>>,
    play: impl Fn(&ModelRequest) -> ModelResponse + Send + Sync + 'static,
) -> Model {
    let asked = Arc::clone(asked);
    let play: Arc<dyn Fn(&ModelRequest) -> ModelResponse + Send + Sync> = Arc::new(play);
    Arc::new(move || {
        Some(Box::new(Plays {
            play: Arc::clone(&play),
            asked: Arc::clone(&asked),
        }) as Box<dyn tilo_agent_session::model::PlanTurns>)
    })
}

/// The agents the server works for `workspaces`, answered by `model`.
fn worked(
    workspaces: &Workspaces,
    actions: &tilo_agent_session::workspace_actions::WorkspaceActions,
    model: Model,
) -> ServerAgents {
    ServerAgents::for_test(workspaces.clone(), actions.clone(), Some(model))
}

/// Whether `request` is a call of a sub-agent's turn: what it is told before everything says so.
fn of_sub_agent(request: &ModelRequest) -> bool {
    said_in(request).contains("You are a sub-agent")
}

/// Whether a tool has answered in `request` already.
fn answered_a_tool(request: &ModelRequest) -> bool {
    request
        .messages
        .iter()
        .any(|message| matches!(message.role, Role::Tool))
}

/// What the tools answered in `request`, as the model was shown it.
fn tools_said(request: &ModelRequest) -> String {
    format!(
        "{:?}",
        request
            .messages
            .iter()
            .filter(|message| matches!(message.role, Role::Tool))
            .collect::<Vec<_>>()
    )
}

/// A model turn that reads what stands at `path`, as the agent tools name it.
fn reads(path: &str) -> ModelResponse {
    answer(
        vec![Content::ToolCall(ToolCall {
            id: "read-1".into(),
            name: "tilo_read".into(),
            input: serde_json::json!({"path": path}),
            extra_content: None,
        })],
        StopReason::ToolCalls,
        1_000,
    )
}

/// A model turn that invokes what `path` names with `args`, as the agent tools name it: the
/// call numbered `step` of its turn.
fn does(step: usize, path: &str, args: &serde_json::Value) -> ModelResponse {
    answer(
        vec![Content::ToolCall(ToolCall {
            id: format!("do-{step}"),
            name: "tilo_do".into(),
            input: serde_json::json!({"path": path, "args": args}),
            extra_content: None,
        })],
        StopReason::ToolCalls,
        1_000,
    )
}

/// What the tools answered in `request`, in the order asked: each answer's words, and whether
/// it was a failure.
fn tool_answers(request: &ModelRequest) -> Vec<(String, bool)> {
    request
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|content| match content {
            Content::ToolResult(result) => Some((result.content.clone(), result.is_error)),
            _ => None,
        })
        .collect()
}

/// The workspace's list of its agents, as the workspace keeps it.
async fn roster(workspaces: &Workspaces, workspace: &str) -> WorkspaceAgents {
    let document = collab_types::workspace_document_id(ui::tilo_workspace_agent::KIND);
    let bytes = workspaces
        .resource_document(workspace, ui::tilo_workspace_agent::OWNER, &document)
        .await
        .unwrap()
        .unwrap();
    WorkspaceAgents::read(&automerge::AutoCommit::load(&bytes).unwrap()).unwrap()
}

/// Apply `invoke` on the workspace's list of its agents as `agent` acting for `person`, or
/// with no agent as `person` themselves.
async fn on_roster(
    workspaces: &Workspaces,
    workspace: &str,
    (agent, person): (Option<&str>, &str),
    invoke: Invoke<WorkspaceAgents>,
) -> Result<(), String> {
    let document = collab_types::workspace_document_id(ui::tilo_workspace_agent::KIND);
    workspaces
        .author_application(
            workspace,
            workspaces.governor(),
            &tilo_agent_session::AppCatalogue::native(),
            ui::tilo_workspace_agent::OWNER,
            &document,
            |doc| {
                let replica = Replica::<WorkspaceAgents>::from_doc(std::mem::take(doc));
                let mut replica = match agent {
                    Some(agent) => replica.as_agent_for(agent, person),
                    None => replica.as_author(person),
                }
                .in_app(ui::tilo_workspace_agent::OWNER, "Assistant");
                replica.set_now_ms(tilo_clock::unix_now_ms_i64());
                let applied = replica
                    .apply(invoke)
                    .map(drop)
                    .map_err(|error| error.to_string());
                std::mem::swap(doc, replica.doc_mut());
                applied
            },
        )
        .await
        .map_err(|error| error.to_string())
}

/// `start-sub-agent` for the job `job`, under the name `name`, its id `id`.
fn start(name: &str, job: &str) -> Invoke<WorkspaceAgents> {
    WorkspaceAgents::START_SUB_AGENT.with(workspace_agents::StartSubAgent {
        sub_agent: NewSubAgent {
            name: name.to_owned(),
            job: job.to_owned(),
            role: Some("Researcher".to_owned()),
            ..NewSubAgent::default()
        },
    })
}

/// `note-sub-agent`: `text` for the sub-agent `agent`.
fn note(agent: &str, text: &str) -> Invoke<WorkspaceAgents> {
    WorkspaceAgents::NOTE_SUB_AGENT.with(workspace_agents::NoteSubAgent {
        agent: Id::from_raw(agent.to_owned()),
        text: NonBlank::parse(text).unwrap(),
    })
}

/// `stop-sub-agent` for the sub-agent `agent`.
fn stop(agent: &str) -> Invoke<WorkspaceAgents> {
    WorkspaceAgents::STOP_SUB_AGENT.with(workspace_agents::StopSubAgent {
        agent: Id::from_raw(agent.to_owned()),
    })
}

/// The sub-agent `agent` once the list says it stands `stands`.
async fn stands_as(workspaces: &Workspaces, workspace: &str, agent: &str, stands: Stands) -> Agent {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let found = roster(workspaces, workspace)
            .await
            .get(&Id::from_raw(agent))
            .cloned();
        let now = found
            .as_ref()
            .and_then(|found| found.sub.as_ref())
            .map(|sub| sub.stands);
        if now == Some(stands) {
            return found.unwrap();
        }
        assert!(
            Instant::now() < deadline,
            "{agent} never stood {stands}: it stands {now:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// The person's own agent starts the sub-agent `name` for `job`: `start-sub-agent`, applied on
/// the list as the agent acting for its person.
async fn starts(workspaces: &Workspaces, workspace: &str, person: &str, (name, job): (&str, &str)) {
    on_roster(
        workspaces,
        workspace,
        (Some(SCOUT), person),
        start(name, job),
    )
    .await
    .expect("the person's own agent starts a sub-agent");
}

/// Every ask the agent package holds in Chat's record now, as `person`'s devices read it.
async fn asked_now(workspaces: &Workspaces, workspace: &str, person: &str) -> Vec<asks::Asked> {
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

/// The asks of the agent `agent`, oldest first.
async fn asks_of(
    workspaces: &Workspaces,
    workspace: &str,
    person: &str,
    agent: &str,
) -> Vec<asks::Asked> {
    asked_now(workspaces, workspace, person)
        .await
        .into_iter()
        .filter(|asked| asked.agent == MemberId::agent(agent))
        .collect()
}

/// The ask `at` once its newest reply ended.
async fn answered(
    workspaces: &Workspaces,
    workspace: &str,
    person: &str,
    at: &asks::ResourceRef,
) -> asks::Asked {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let found = asked_now(workspaces, workspace, person)
            .await
            .into_iter()
            .find(|asked| asked.at.same(at));
        if let Some(found) = &found
            && found
                .latest()
                .is_some_and(|reply| reply.state != AskState::Working)
        {
            return found.clone();
        }
        assert!(
            Instant::now() < deadline,
            "the ask was never answered: {found:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// The asks the sub-agent `agent` made of its starter with its news, once there are `count`.
async fn news_of(
    workspaces: &Workspaces,
    workspace: &str,
    person: &str,
    (agent, count): (&str, usize),
) -> Vec<asks::Asked> {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let news: Vec<asks::Asked> = asked_now(workspaces, workspace, person)
            .await
            .into_iter()
            .filter(|asked| {
                matches!(&asked.who, Some(asks::Actor::Member(member)) if member.agent_id() == Some(agent))
            })
            .collect();
        if news.len() >= count {
            return news;
        }
        assert!(
            Instant::now() < deadline,
            "{agent} never told its starter {count} times: {news:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// `person` mentions the agent `agent`, called `name`, in a channel of Chat with `words`: the
/// ask their device's host makes from the mention, as theirs.
async fn mentions(
    workspaces: &Workspaces,
    workspace: &str,
    person: &str,
    (agent, name, words): (&str, &str, &str),
) {
    let packages = asks::chat_packages().unwrap();
    let them = asks::Actor::Member(MemberId::person(person.to_owned()));
    let chat = packages.app().to_owned();
    let (agent, name, words) = (agent.to_owned(), name.to_owned(), words.to_owned());
    workspaces
        .with_tl_record(workspace, &packages, them.clone(), move |opened| {
            let all = opened
                .host()
                .root_of(&chat)
                .map_err(|error| error.to_string())?;
            let channel = opened.invoke(
                &them,
                (&chat, "new_channel"),
                vec![Value::resource(&all), Value::Text("work".into())],
                None,
            )?;
            opened
                .invoke(
                    &them,
                    (&chat, "send"),
                    vec![
                        channel,
                        asks::mentioning(&MemberId::agent(agent), &name, &words),
                        Value::Opt(None),
                    ],
                    None,
                )
                .map(drop)
        })
        .await
        .unwrap();
}

/// Answer `turn` and wait until it ended.
async fn answers(
    agents: &ServerAgents,
    workspace: &str,
    turn: &tilo_server::workspace_agents::Turn,
) {
    tokio::time::timeout(Duration::from_secs(60), agents.answer(workspace, turn))
        .await
        .expect("the turn ends")
        .unwrap();
}

/// What `request` was told, every part of it.
fn told_all(request: &ModelRequest) -> String {
    format!("{:?}", request.messages)
}

/// The person's own agent starts a sub-agent outside a turn. With no device on, the server asks
/// it its job in its starter's name, once however often it looks; the sub-agent's turn is told
/// its job and nothing around it, its answer is its own reply, its item says it is done, and its
/// starter is asked with the news, made from that reply, and reads it there.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sub_agent_answers_its_job_as_its_reply_and_its_starter_reads_it_there() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let (server, actions) = crate::harness::hosted::serving_documents_with_actions(
        &root,
        &host,
        state.path(),
        crate::common::catalogue,
    )
    .await;
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let page = Browser::open_at(&server, &browser, &host, &workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    make_agent(&page, &workspaces, &workspace, &person, None).await;

    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = worked(
        &workspaces,
        &actions,
        plays(&asked, |request| {
            if of_sub_agent(request) {
                answer(
                    vec![Content::Text {
                        text: "The deck is in Files, under Decks.".into(),
                    }],
                    StopReason::EndTurn,
                    1_000,
                )
            } else {
                answer(
                    vec![Content::Text {
                        text: "Ada, the deck is in Files.".into(),
                    }],
                    StopReason::EndTurn,
                    1_000,
                )
            }
        }),
    );

    starts(
        &workspaces,
        &workspace,
        &person,
        ("Researcher", "Find the deck for Monday."),
    )
    .await;
    let researcher = roster(&workspaces, &workspace)
        .await
        .agents
        .iter()
        .find(|agent| agent.name == "Researcher")
        .map(|agent| agent.id.0.clone())
        .expect("the list holds the sub-agent");

    // Its job is asked of it in its starter's name, once however often the server looks.
    let turns = agents.turns(&workspace).await.unwrap();
    let again = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "its job is its one turn: {turns:?}");
    assert_eq!(again.len(), 1, "{again:?}");
    let job = asks_of(&workspaces, &workspace, &person, &researcher).await;
    assert_eq!(job.len(), 1, "a cause asks once: {job:?}");
    assert_eq!(job[0].state, AskState::Waiting);
    assert!(
        matches!(&job[0].who, Some(asks::Actor::Member(member)) if member.agent_id() == Some(SCOUT)),
        "its starter asked it: {:?}",
        job[0].who
    );
    let turn = &turns[0];
    assert_eq!(turn.agent.id.0, researcher);
    assert_eq!(turn.person, person, "it works for its starter's person");
    assert_eq!(turn.request, "Find the deck for Monday.");
    let line = turn
        .line
        .as_ref()
        .expect("a sub-agent's turn knows its line");
    assert!(line.job && line.from_starter, "{line:?}");
    assert_eq!(line.root.id.0, SCOUT, "it spends on its starter's day");

    // Its answer is its own reply, and its item says it is done.
    answers(&agents, &workspace, turn).await;
    let job = answered(&workspaces, &workspace, &person, &job[0].at).await;
    assert_eq!(job.state, AskState::Done, "{job:?}");
    let reply = job.latest().unwrap();
    assert!(
        reply.body.contains("The deck is in Files, under Decks."),
        "{}",
        reply.body
    );
    stands_as(&workspaces, &workspace, &researcher, Stands::Done).await;
    let said_to_it = asked.lock().unwrap().clone();
    assert!(
        said_to_it
            .iter()
            .filter(|request| of_sub_agent(request))
            .all(|request| told_all(request).contains("Find the deck for Monday.")),
        "it was told its job"
    );

    // Its starter is asked with the news, made from that reply, and reads the reply there.
    let news = news_of(&workspaces, &workspace, &person, (&researcher, 1)).await;
    assert_eq!(news.len(), 1, "{news:?}");
    assert_eq!(news[0].agent, MemberId::agent(SCOUT));
    assert!(
        news[0]
            .from
            .as_ref()
            .is_some_and(|from| from.same(&reply.at)),
        "the news is made from the sub-agent's reply: {:?}",
        news[0].from
    );
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(
        turns.len(),
        1,
        "the news is its starter's one turn: {turns:?}"
    );
    let told = &turns[0];
    assert_eq!(told.agent.id.0, SCOUT);
    assert!(
        told.request.contains("stands: done")
            && told.request.contains("The deck is in Files, under Decks."),
        "its starter reads what it answered: {}",
        told.request
    );
    assert!(told.line.is_none(), "a person's own agent has no line");
    answers(&agents, &workspace, told).await;
    let news = answered(&workspaces, &workspace, &person, &news[0].at).await;
    assert_eq!(news.state, AskState::Done);
    assert!(
        agents.turns(&workspace).await.unwrap().is_empty(),
        "nothing more is owed, and the ended work tells nobody twice"
    );
    assert_eq!(
        news_of(&workspaces, &workspace, &person, (&researcher, 1))
            .await
            .len(),
        1
    );
    page.peer.close();
}

/// A note its starter gives it asks it again, in the starter's name, made from its job's ask.
/// A stop on its item takes back what its line asked before it and nobody took; a note given
/// after the stop is new work. Its person asks it directly, and is answered too; a member who is
/// not its person is not.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_note_asks_it_again_and_a_stop_takes_back_what_waits() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let (server, actions) = crate::harness::hosted::serving_documents_with_actions(
        &root,
        &host,
        state.path(),
        crate::common::catalogue,
    )
    .await;
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let page = Browser::open_at(&server, &browser, &host, &workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    make_agent(&page, &workspaces, &workspace, &person, None).await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = worked(
        &workspaces,
        &actions,
        looks_then_answers(1_000, Arc::clone(&asked)),
    );

    starts(
        &workspaces,
        &workspace,
        &person,
        ("Sorter", "Sort the notes."),
    )
    .await;
    let sorter = roster(&workspaces, &workspace)
        .await
        .agents
        .iter()
        .find(|agent| agent.name == "Sorter")
        .map(|agent| agent.id.0.clone())
        .unwrap();
    assert_eq!(agents.turns(&workspace).await.unwrap().len(), 1, "its job");

    // A note asks it again, made from its job's ask.
    on_roster(
        &workspaces,
        &workspace,
        (Some(SCOUT), &person),
        note(&sorter, "Leave the drafts alone."),
    )
    .await
    .unwrap();
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 2, "its job and the note: {turns:?}");
    let noted = turns
        .iter()
        .find(|turn| turn.line.as_ref().is_some_and(|line| !line.job))
        .expect("the note's turn");
    assert_eq!(noted.request, "A note from Scout: Leave the drafts alone.");
    let held = asks_of(&workspaces, &workspace, &person, &sorter).await;
    assert_eq!(held.len(), 2, "{held:?}");
    assert!(
        held[1]
            .from
            .as_ref()
            .is_some_and(|from| from.same(&held[0].at)),
        "the note is made from its job's ask: {held:?}"
    );

    // A stop takes back both, as nobody took them yet.
    on_roster(
        &workspaces,
        &workspace,
        (Some(SCOUT), &person),
        stop(&sorter),
    )
    .await
    .unwrap();
    assert!(
        agents.turns(&workspace).await.unwrap().is_empty(),
        "what it was asked before the stop is owed nothing"
    );
    let held = asks_of(&workspaces, &workspace, &person, &sorter).await;
    assert!(
        held.iter().all(|asked| asked.state == AskState::Withdrawn),
        "its asker took back what waited: {held:?}"
    );

    // A note after the stop is new work, and its answer is its reply.
    tokio::time::sleep(Duration::from_millis(5)).await;
    on_roster(
        &workspaces,
        &workspace,
        (Some(SCOUT), &person),
        note(&sorter, "Sort only this week's."),
    )
    .await
    .unwrap();
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "the new note alone: {turns:?}");
    assert_eq!(
        turns[0].request,
        "A note from Scout: Sort only this week's."
    );
    answers(&agents, &workspace, &turns[0]).await;
    stands_as(&workspaces, &workspace, &sorter, Stands::Done).await;

    // Its person asks it in a channel of Chat: their own words, and nothing around them.
    mentions(
        &workspaces,
        &workspace,
        &person,
        (&sorter, "Sorter", " how far are you?"),
    )
    .await;
    let turns: Vec<_> = agents
        .turns(&workspace)
        .await
        .unwrap()
        .into_iter()
        .filter(|turn| turn.agent.id.0 == sorter)
        .collect();
    assert_eq!(turns.len(), 1, "its person's ask: {turns:?}");
    assert!(turns[0].typed, "its person asked it themselves");
    assert!(
        turns[0].request.contains("how far are you?") && !turns[0].request.contains("In \""),
        "what its person wrote, and nothing around it: {}",
        turns[0].request
    );
    assert!(
        turns[0]
            .line
            .as_ref()
            .is_some_and(|line| !line.from_starter),
        "its person's ask tells its starter nothing when it ends"
    );
    let _ = asked;
    page.peer.close();
}

/// A team from inside a turn: the person's own agent starts and steers its sub-agents from
/// its own turn, and a lead reads the team under it.
#[path = "hosted_sub_agent_teams.rs"]
mod teams;

/// The workspace’s shared Skills actions.
#[path = "hosted_skills.rs"]
mod skills;

/// What a look read before a turn ended, once that turn has ended.
#[path = "hosted_turn_ends.rs"]
mod turn_ends;

/// A model call its provider refuses for a moment is asked again, with the turn's work kept.
#[path = "hosted_turn_asks_again.rs"]
mod asks_again;
