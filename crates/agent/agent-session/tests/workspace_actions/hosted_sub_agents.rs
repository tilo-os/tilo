//! Sub-agents on the server, with no device of the person running anything: the person's own
//! agent starts one, it works in a conversation of its own, a note reaches it at its next step,
//! several work at once, its person writes into one and stops one, and when work its starter
//! gave it ends, its starter is told. And the one-way rule as a permission: what a sub-agent's
//! own turn is answered when it reads the thread of the agent that started it.
use super::*;
use chat::chat::{Message as Kept, reply_id};
use tilo_server::workspaces::Workspaces;
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
) -> HostAgents {
    HostAgents::new(workspaces.clone(), actions.clone(), Some(model))
}

/// What Chat's `post` is given for `text` in `conversation`, and its `ask-agent` with a `cause`.
fn said(conversation: &str, text: &str, cause: Option<serde_json::Value>) -> serde_json::Value {
    let mut said = serde_json::json!({"channel": conversation, "text": text});
    if let (Some(cause), Some(said)) = (cause, said.as_object_mut()) {
        said.insert("cause".to_owned(), cause);
    }
    said
}

/// What a call of Chat's `stream-output` says to put `text` at the end of `reply` in
/// `conversation`: where, which reply, and the piece. The piece names no stream, so Chat
/// itself would refuse it as no piece at all, in other words than whoever serves the turn
/// refuses a call: a call that was let through to Chat shows in its answer's words.
fn grown(conversation: &str, reply: &str, text: &str) -> serde_json::Value {
    let mut grown = said(conversation, "", None);
    if let Some(grown) = grown.as_object_mut() {
        grown.remove("text");
        grown.insert("reply".to_owned(), serde_json::json!(reply));
        grown.insert(
            "event".to_owned(),
            serde_json::json!({"sequence": 1, "event": {"type": "text_delta", "text": text}}),
        );
    }
    grown
}

/// The conversation `message` is in, as Chat names it.
fn conversation_id(message: &Kept) -> &Id {
    &message.channel
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
        .resource_document(workspace, CHAT, &document)
        .await
        .unwrap()
        .unwrap();
    WorkspaceAgents::read(&automerge::AutoCommit::load(&bytes).unwrap()).unwrap()
}

/// Apply `invoke` on the workspace's list of its agents as `agent` acting for `person`, or
/// with no agent as `person` themselves, where the Chat of a person's device applies it. A
/// hosted turn's own tools do not reach this list: the server's action service serves an app's
/// own document and the parts of its chains, not the document every app of the workspace opens.
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
            CHAT,
            &document,
            |doc| {
                let replica = Replica::<WorkspaceAgents>::from_doc(std::mem::take(doc));
                let mut replica = match agent {
                    Some(agent) => replica.as_agent_for(agent, person),
                    None => replica.as_author(person),
                }
                .in_app(CHAT, "Chat");
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

/// `start-sub-agent` for the job `job`, under the name `name`.
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

/// The messages of `conversation`, as the workspace keeps them.
async fn said_there(
    workspaces: &Workspaces,
    workspace: &str,
    conversation: &str,
) -> Vec<(Id, Kept)> {
    conversation_part(workspaces, workspace)
        .await
        .messages
        .iter()
        .filter(|(_, message)| conversation_id(message).as_str() == conversation)
        .map(|(id, message)| (id.clone(), message.clone()))
        .collect()
}

/// The messages of `conversation` that carry a cause of `kind`, once there are `count` of them.
async fn caused(
    workspaces: &Workspaces,
    workspace: &str,
    (conversation, kind): (&str, &str),
    count: usize,
) -> Vec<(Id, Kept)> {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let found: Vec<_> = said_there(workspaces, workspace, conversation)
            .await
            .into_iter()
            .filter(|(_, message)| {
                message
                    .cause
                    .as_ref()
                    .is_some_and(|cause| cause.kind.0 == kind)
            })
            .collect();
        if found.len() >= count {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "{conversation} never held {count} of {kind}: {}",
            found.len()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// `agent`'s reply to `message`, once its work ended.
async fn reply_of(workspaces: &Workspaces, workspace: &str, message: &Id, agent: &str) -> Kept {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let part = conversation_part(workspaces, workspace).await;
        let reply = part
            .messages
            .get(&reply_id(message, &MemberId::agent(agent)));
        if let Some(reply) = reply
            && !chat::summons::unanswered(Some(reply))
        {
            return reply.clone();
        }
        assert!(
            Instant::now() < deadline,
            "{agent}'s reply to {message} never ended: {:?}",
            reply.map(|reply| reply.work.clone())
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// `caller` writes `text` into `conversation`, as a person's page does: the message's id.
async fn writes_in(
    actions: &tilo_agent_session::workspace_actions::WorkspaceActions,
    workspaces: &Workspaces,
    workspace: &str,
    caller: &Caller,
    (conversation, text): (&str, &str),
) -> Id {
    chat_action(
        actions,
        workspace,
        caller,
        ("", &ui::resource::chain_resource_id(&chat::parts::chain())),
        "post",
        said(conversation, text, None),
    )
    .await
    .expect("the person writes into the conversation");
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let held = said_there(workspaces, workspace, conversation).await;
        if let Some((id, _)) = held.iter().find(|(_, message)| message.text == text) {
            return id.clone();
        }
        assert!(Instant::now() < deadline, "the server never holds {text:?}");
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

/// The person's own agent starts a sub-agent. With no device on, the server writes the job
/// into a conversation of the sub-agent's own, works it there, says on its item how it stands, and
/// tells its starter when it ends. What the sub-agent reads of Chat is refused, the thread of
/// the agent that started it first of all, while the same read by that agent is answered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sub_agent_works_in_a_conversation_of_its_own_and_reads_nothing_of_its_starters() {
    team!(root, browser, actions, workspaces, workspace, page, person);
    let ada = themselves(&root, &browser, &workspace);
    let thread = chat::Chat::direct_id([MemberId::person(person.clone()), MemberId::agent(SCOUT)]);

    // Every turn reads the chat's messages first, the person's own agent and the sub-agent
    // alike, then answers.
    let chat_messages = "chat/Messages".to_owned();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let path = chat_messages.clone();
    let agents = worked(
        &workspaces,
        &actions,
        plays(&asked, move |request| {
            let told = said_in(request);
            let news = told.contains("Your sub-agent");
            if !answered_a_tool(request) && !news {
                return reads(&path);
            }
            if of_sub_agent(request) {
                says("I could not read the chat. The deck is not in Files.")
            } else if news {
                says("Find the deck is done: the deck is not in Files.")
            } else {
                says("Noted.")
            }
        }),
    );

    // The main thread holds something only it holds, and the person's own agent reads it.
    let secret = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (thread.as_str(), "The launch's code word is heron."),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    let noted = reply_of(&workspaces, &workspace, &secret, SCOUT).await;
    assert!(noted.text.contains("Noted"), "{}", noted.text);
    let main_read = tools_said(&asked.lock().unwrap()[1]);
    assert!(
        main_read.contains("heron"),
        "the person's own agent reads its thread: {main_read}"
    );

    // Only an agent starts a sub-agent: the person asks their own agent for one.
    let refused = on_roster(
        &workspaces,
        &workspace,
        (None, person.as_str()),
        start("Find the deck", "Find the Q3 board deck in Files."),
    )
    .await
    .expect_err("a person starts none themselves");
    assert!(refused.contains("ask your own agent"), "{refused}");
    starts(
        &workspaces,
        &workspace,
        &person,
        ("Find the deck", "Find the Q3 board deck in Files."),
    )
    .await;
    let started = stands_as(&workspaces, &workspace, "find-the-deck", Stands::Working).await;
    let sub = started.sub.clone().unwrap();
    assert_eq!(started.keeper, person, "it works for the same person");
    assert!(started.on_host);
    assert_eq!(
        (
            sub.started_by.as_str(),
            sub.role.as_str(),
            sub.kind.as_str()
        ),
        (SCOUT, "Researcher", "subtask")
    );
    let own = chat::Chat::direct_id([
        MemberId::person(person.clone()),
        MemberId::agent("find-the-deck"),
    ]);
    assert_eq!(sub.conversation, own.as_str(), "a conversation of its own");

    // The server carries the job into that conversation, as the words of the agent that gave it.
    agents.look(&workspace).await.unwrap();
    let jobs = caused(&workspaces, &workspace, (own.as_str(), "job"), 1).await;
    let (job, written) = &jobs[0];
    assert_eq!(written.text, "Find the Q3 board deck in Files.");
    assert_eq!(
        written.author,
        MemberId::agent(SCOUT),
        "the job is its starter's own words"
    );
    let cause = written.cause.as_ref().unwrap();
    assert_eq!(cause.label, "Scout");
    assert_eq!(cause.person, MemberId::person(person.clone()));
    // Looking again writes it no second time.
    agents.look(&workspace).await.unwrap();

    // The sub-agent works there, on the server, and its answer stands under its job.
    let answered = reply_of(&workspaces, &workspace, job, "find-the-deck").await;
    assert_eq!(answered.author, MemberId::agent("find-the-deck"));
    assert_eq!(*conversation_id(&answered), own);
    assert!(
        answered.text.contains("The deck is not in Files"),
        "{}",
        answered.text
    );
    assert_eq!(
        caused(&workspaces, &workspace, (own.as_str(), "job"), 1)
            .await
            .len(),
        1,
        "one job, however often the server looked"
    );

    // THE ONE-WAY RULE. Its own turn read the chat's messages and was refused: nothing it was
    // ever shown holds a word of its starter's thread.
    let subs: Vec<ModelRequest> = asked
        .lock()
        .unwrap()
        .iter()
        .filter(|request| of_sub_agent(request))
        .cloned()
        .collect();
    assert_eq!(subs.len(), 2, "one call to read, one to answer");
    let told = said_in(&subs[0]);
    assert!(
        told.contains("Find the Q3 board deck in Files.") && told.contains("Scout started you"),
        "it is told its job and who started it: {told}"
    );
    // The same read the person's own agent was answered is refused to the sub-agent as not allowed.
    let refused = tools_said(&subs[1]);
    assert_ne!(
        refused, main_read,
        "it is not answered what its starter was"
    );
    assert!(
        refused.contains("not_permitted: not allowed"),
        "its read of the chat is refused as not allowed: {refused}"
    );
    for request in &subs {
        let shown = format!("{:?}", request.messages);
        assert!(
            !shown.contains("heron"),
            "a sub-agent is shown nothing of its starter's thread: {shown}"
        );
    }

    // Its item says how it stands, in a line.
    let done = stands_as(&workspaces, &workspace, "find-the-deck", Stands::Done).await;
    assert_eq!(
        done.sub.unwrap().line,
        "I could not read the chat. The deck is not in Files."
    );

    // Its starter is told in the starter's own conversation, and says so there.
    let news = caused(&workspaces, &workspace, (thread.as_str(), "news"), 1).await;
    let (told, said) = &news[0];
    assert_eq!(said.author, MemberId::agent("find-the-deck"));
    assert!(
        said.text.contains("stands: done") && said.text.contains("The deck is not in Files"),
        "{}",
        said.text
    );
    agents.look(&workspace).await.unwrap();
    let line = reply_of(&workspaces, &workspace, told, SCOUT).await;
    assert!(line.text.contains("Find the deck is done"), "{}", line.text);

    // What the sub-agent's calls cost is counted on the day of the person's own agent.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let list = roster(&workspaces, &workspace).await;
        let spent = |agent: &str| {
            list.get(&Id::from_raw(agent))
                .and_then(|agent| agent.spent.clone())
                .map(|spent| spent.cost_micros)
        };
        if spent(SCOUT) == Some(5_000) {
            assert_eq!(
                spent("find-the-deck"),
                None,
                "a sub-agent has no day of its own"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "five calls on the main agent's day: {:?}",
            spent(SCOUT)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    page.peer.close();
}

/// A note its starter gives a sub-agent while it works reaches it at its next step: one turn,
/// which goes on with the note, not a second turn beside it. And its person writes into its
/// conversation and is answered there, which its starter is not told.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_note_reaches_a_working_sub_agent_at_its_next_step_and_its_person_writes_into_it() {
    team!(root, browser, actions, workspaces, workspace, page, person);
    let ada = themselves(&root, &browser, &workspace);
    let thread = chat::Chat::direct_id([MemberId::person(person.clone()), MemberId::agent(SCOUT)]);
    let own = chat::Chat::direct_id([
        MemberId::person(person.clone()),
        MemberId::agent("usage-drop"),
    ]);

    // The sub-agent's first step is held, so the note is given while it works.
    let hold = Arc::new(Hold::default());
    let _opens = Opens(Arc::clone(&hold));
    let held = Arc::clone(&hold);
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = worked(
        &workspaces,
        &actions,
        plays(&asked, move |request| {
            if !of_sub_agent(request) {
                return says("Told.");
            }
            if !answered_a_tool(request) {
                held.pass();
                return looks();
            }
            let told = said_in(request);
            if told.contains("only Safari") {
                says("Yes, only Safari.")
            } else if told.contains("Tuesday") {
                says("Done, sign-in included.")
            } else {
                says("Done.")
            }
        }),
    );
    starts(
        &workspaces,
        &workspace,
        &person,
        ("Usage drop", "Find out why daily use fell."),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    let jobs = caused(&workspaces, &workspace, (own.as_str(), "job"), 1).await;
    agents.look(&workspace).await.unwrap();
    until("the sub-agent's first step is under way", || {
        asked.lock().unwrap().iter().any(of_sub_agent)
    })
    .await;

    // Its starter gives it a note. The server writes it into the conversation as the starter's
    // words, and hands it to the turn that is going.
    on_roster(
        &workspaces,
        &workspace,
        (Some(SCOUT), person.as_str()),
        WorkspaceAgents::NOTE_SUB_AGENT.with(workspace_agents::NoteSubAgent {
            agent: Id::from_raw("usage-drop".to_owned()),
            text: NonBlank::parse("Also check Tuesday's sign-in change.").unwrap(),
        }),
    )
    .await
    .expect("its starter gives it a note");
    agents.look(&workspace).await.unwrap();
    let notes = caused(&workspaces, &workspace, (own.as_str(), "note"), 1).await;
    let (note, written) = &notes[0];
    assert_eq!(written.text, "Also check Tuesday's sign-in change.");
    assert_eq!(written.author, MemberId::agent(SCOUT));
    agents.look(&workspace).await.unwrap();
    hold.open();

    // One turn: it went on with the note, and its answer stands under it.
    let answered = reply_of(&workspaces, &workspace, note, "usage-drop").await;
    assert_eq!(answered.text, "Done, sign-in included.");
    let left = reply_of(&workspaces, &workspace, &jobs[0].0, "usage-drop").await;
    assert!(
        left.text.contains("going on"),
        "the job's own reply says the answer follows below: {}",
        left.text
    );
    let done = stands_as(&workspaces, &workspace, "usage-drop", Stands::Done).await;
    assert_eq!(done.sub.unwrap().line, "Done, sign-in included.");
    let subs: Vec<ModelRequest> = asked
        .lock()
        .unwrap()
        .iter()
        .filter(|request| of_sub_agent(request))
        .cloned()
        .collect();
    assert_eq!(
        subs.len(),
        2,
        "the note started no second turn beside the first"
    );
    assert!(
        said_in(&subs[1]).contains("Also check Tuesday's sign-in change."),
        "its next step is told the note: {}",
        said_in(&subs[1])
    );
    // Its starter is told once, where the starter talks with its person.
    let news = caused(&workspaces, &workspace, (thread.as_str(), "news"), 1).await;
    agents.look(&workspace).await.unwrap();
    reply_of(&workspaces, &workspace, &news[0].0, SCOUT).await;

    // The person writes into it and is answered there.
    let typed = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (own.as_str(), "is it only Safari?"),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    let answered = reply_of(&workspaces, &workspace, &typed, "usage-drop").await;
    assert_eq!(answered.text, "Yes, only Safari.");
    assert_eq!(*conversation_id(&answered), own);
    let again = stands_as(&workspaces, &workspace, "usage-drop", Stands::Done).await;
    let deadline = tilo_test_env::hang_deadline();
    let mut again = again;
    while again.sub.as_ref().unwrap().line != "Yes, only Safari." {
        assert!(
            Instant::now() < deadline,
            "its line follows its last answer"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
        again = stands_as(&workspaces, &workspace, "usage-drop", Stands::Done).await;
    }
    // What a sub-agent answers its person is theirs: its starter is told nothing more.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        caused(&workspaces, &workspace, (thread.as_str(), "news"), 1)
            .await
            .len(),
        1,
        "one piece of news, for the work its starter gave it"
    );
    page.peer.close();
}

/// Several sub-agents work at once, each in its own conversation, and a stop on the list
/// ends the one it names while the others go on.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn several_sub_agents_work_at_once_and_a_stop_ends_the_one_it_names() {
    team!(root, browser, actions, workspaces, workspace, page, person);
    let thread = chat::Chat::direct_id([MemberId::person(person.clone()), MemberId::agent(SCOUT)]);
    let own = |agent: &str| {
        chat::Chat::direct_id([MemberId::person(person.clone()), MemberId::agent(agent)])
    };

    let hold = Arc::new(Hold::default());
    let _opens = Opens(Arc::clone(&hold));
    let held = Arc::clone(&hold);
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = worked(
        &workspaces,
        &actions,
        plays(&asked, move |request| {
            if !of_sub_agent(request) {
                return says("Told.");
            }
            if !answered_a_tool(request) {
                held.pass();
                return looks();
            }
            says("Done.")
        }),
    );
    starts(
        &workspaces,
        &workspace,
        &person,
        ("Research A", "Look into company A."),
    )
    .await;
    starts(
        &workspaces,
        &workspace,
        &person,
        ("Research B", "Look into company B."),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    let job_a = caused(
        &workspaces,
        &workspace,
        (own("research-a").as_str(), "job"),
        1,
    )
    .await;
    let job_b = caused(
        &workspaces,
        &workspace,
        (own("research-b").as_str(), "job"),
        1,
    )
    .await;
    agents.look(&workspace).await.unwrap();

    // Both are at their first step at the same time.
    until("both sub-agents work at once", || {
        let asked = asked.lock().unwrap();
        ["Look into company A.", "Look into company B."]
            .iter()
            .all(|job| asked.iter().any(|request| said_in(request).contains(job)))
    })
    .await;
    for agent in ["research-a", "research-b"] {
        stands_as(&workspaces, &workspace, agent, Stands::Working).await;
    }

    // Their person stops one. Nobody but its person or whoever steers it may: the other
    // sub-agent is refused.
    let refused = on_roster(
        &workspaces,
        &workspace,
        (Some("research-b"), person.as_str()),
        stop("research-a"),
    )
    .await
    .expect_err("one sub-agent does not stop another");
    assert!(refused.contains("steers"), "{refused}");
    on_roster(
        &workspaces,
        &workspace,
        (None, person.as_str()),
        stop("research-a"),
    )
    .await
    .expect("its person stops it");
    stands_as(&workspaces, &workspace, "research-a", Stands::Stopped).await;
    agents.look(&workspace).await.unwrap();
    hold.open();

    // The stopped one ends without an answer and stays stopped; the other finishes.
    let stopped = reply_of(&workspaces, &workspace, &job_a[0].0, "research-a").await;
    assert!(
        !stopped.text.contains("Done."),
        "a stopped turn gives no answer: {}",
        stopped.text
    );
    let finished = reply_of(&workspaces, &workspace, &job_b[0].0, "research-b").await;
    assert_eq!(finished.text, "Done.");
    stands_as(&workspaces, &workspace, "research-b", Stands::Done).await;
    stands_as(&workspaces, &workspace, "research-a", Stands::Stopped).await;

    // Their starter is told of the one that finished, not of the one its person stopped.
    let news = caused(&workspaces, &workspace, (thread.as_str(), "news"), 1).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let news_now = caused(&workspaces, &workspace, (thread.as_str(), "news"), 1).await;
    assert_eq!(news_now.len(), 1, "one piece of news");
    assert_eq!(news[0].1.author, MemberId::agent("research-b"));
    page.peer.close();
}

/// `caller` asks for a sub-agent in `conversation`, through Chat's `ask-agent` under the cause
/// kind `sub-agent`: the job `job`, under the name `name`.
async fn asks_for_one(
    actions: &tilo_agent_session::workspace_actions::WorkspaceActions,
    workspace: &str,
    caller: &Caller,
    conversation: &Id,
    (name, job, key): (&str, &str, &str),
) -> Result<serde_json::Value, String> {
    chat_action(
        actions,
        workspace,
        caller,
        ("", &ui::resource::chain_resource_id(&chat::parts::chain())),
        "ask-agent",
        said(
            conversation.as_str(),
            job,
            Some(serde_json::json!({"kind": "sub-agent", "label": name, "key": key})),
        ),
    )
    .await
}

/// Its person asks for a sub-agent in their own thread with their agent, through the server's
/// action service. The server starts it as that agent, with the words of the ask as its job and
/// the cause's label as its name, and answers the ask itself: no model is asked. Looking again
/// starts nothing twice. What an agent asks under that kind starts nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn its_person_asks_for_a_sub_agent_and_the_server_starts_it() {
    team!(root, browser, actions, workspaces, workspace, page, person);
    let ada = themselves(&root, &browser, &workspace);
    let thread = chat::Chat::direct_id([MemberId::person(person.clone()), MemberId::agent(SCOUT)]);
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = worked(
        &workspaces,
        &actions,
        plays(&asked, |request| {
            if of_sub_agent(request) {
                says("The deck is not in Files.")
            } else {
                says("Told.")
            }
        }),
    );

    asks_for_one(
        &actions,
        &workspace,
        &ada,
        &thread,
        (
            "Find the deck",
            "Find the Q3 board deck in Files.",
            "deck-1",
        ),
    )
    .await
    .expect("its person asks for a sub-agent");
    agents.look(&workspace).await.unwrap();
    let ask = chat::cause::message_id(&MemberId::person(person.clone()), "deck-1");
    let id = format!("sub-{}", &ask.as_str()[..12]);
    let started = stands_as(&workspaces, &workspace, &id, Stands::Working).await;
    let sub = started.sub.clone().unwrap();
    assert_eq!(
        (
            started.name.as_str(),
            started.keeper.as_str(),
            sub.started_by.as_str(),
            sub.job.as_str(),
            sub.kind.as_str()
        ),
        (
            "Find the deck",
            person.as_str(),
            SCOUT,
            "Find the Q3 board deck in Files.",
            "subtask"
        )
    );

    // The server answers the ask in the agent's name, and no model was asked for it.
    let answer = reply_of(&workspaces, &workspace, &ask, SCOUT).await;
    assert!(
        answer
            .text
            .contains("I started the sub-agent \"Find the deck\""),
        "{}",
        answer.text
    );
    assert!(
        asked.lock().unwrap().is_empty(),
        "no model is asked to start one"
    );

    // Its job reaches a conversation of its own and it works there; however often the server
    // looked, one sub-agent was started.
    agents.look(&workspace).await.unwrap();
    let own = chat::Chat::direct_id([
        MemberId::person(person.clone()),
        MemberId::agent(id.as_str()),
    ]);
    let jobs = caused(&workspaces, &workspace, (own.as_str(), "job"), 1).await;
    assert_eq!(jobs[0].1.text, "Find the Q3 board deck in Files.");
    agents.look(&workspace).await.unwrap();
    stands_as(&workspaces, &workspace, &id, Stands::Done).await;
    let sub_agents =
        |list: &WorkspaceAgents| list.agents.iter().filter(|agent| agent.is_sub()).count();
    assert_eq!(sub_agents(&roster(&workspaces, &workspace).await), 1);

    // An agent's ask under that kind starts nothing: the sub-agent asks in its starter's thread.
    // Outside a turn nothing is taken in a sub-agent's name but what its own rules let it, and
    // asking an agent is none of that.
    let outside = ada.clone().delegated_to(id.clone()).named("Find the deck");
    let refused = asks_for_one(
        &actions,
        &workspace,
        &outside,
        &thread,
        ("Another", "Do more.", "more-0"),
    )
    .await
    .expect_err("outside a turn a sub-agent's name asks nobody");
    assert!(refused.contains("may not invoke"), "{refused}");
    // What the server writes for a sub-agent it writes on the turn it worked, with the bound its
    // keeper set: an ask made that way is taken, and starts nothing either.
    let beside = outside.on_turn(
        tilo_action_policy::turn::Turn::new(
            chat::summons::conversation_address(&own),
            jobs[0].0.to_string(),
        ),
        Vec::new(),
    );
    asks_for_one(
        &actions,
        &workspace,
        &beside,
        &thread,
        ("Another", "Do more.", "more-1"),
    )
    .await
    .expect("the ask itself is taken");
    agents.look(&workspace).await.unwrap();
    assert_eq!(
        sub_agents(&roster(&workspaces, &workspace).await),
        1,
        "what an agent asks starts none"
    );
    page.peer.close();
}

/// The actions `R` offers that write, by their ids, as the resource itself states them.
fn writes_of<R: ui::tilo_resource::Resource>() -> Vec<String> {
    R::surface()
        .verbs
        .into_iter()
        .filter(|verb| verb.kind != tilo_actions::policy::ActionKind::Read)
        .map(|verb| verb.id)
        .collect()
}

/// Chat's other actions that write, on what the workspace's chat and its messages do not hold:
/// a draft, what was learned, and the workspace's emoji.
const REFUSED_BESIDE: [&str; 7] = [
    "start-draft",
    "learn",
    "correct",
    "forget",
    "add-emoji",
    "remove-emoji",
    "remove-any-emoji",
];

/// What marks a planned call as a read of the path after it, where every other is an action.
const READ: &str = "read ";

/// Whether the tools answered a call with a refusal: the action is not offered to the caller,
/// or the caller may not invoke it.
fn refused_in_words(answer: &str) -> bool {
    [
        "has no action",
        "no such action",
        "may not invoke",
        "not allowed",
    ]
    .iter()
    .any(|said| answer.contains(said))
}

/// A sub-agent writes only into its own conversation. Its turn calls every action of Chat's
/// that writes, each through the turn's own tools and each aimed at the thread of the agent
/// that started it: every one is refused, the three it keeps (its answer, a question to its
/// person and its words while it works) included, since those are taken only where it works.
/// The same call by the person's own agent is carried out, so it is the sub-agent that is
/// refused and not the call. In its own conversation its words are taken as plain words only:
/// not with a mention, not naming a member, not under another message, not with anything
/// attached, and nothing of it asks another agent. Then it says where it is and asks its
/// person there, and the question waits.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sub_agent_writes_only_into_its_own_conversation() {
    team!(root, browser, actions, workspaces, workspace, page, person);
    let ada = themselves(&root, &browser, &workspace);
    // The person is called Ada here, as their page says when it joins: the workspace's member
    // list is whom a name in a message is bound to.
    let named = page
        .peer
        .ask_workspaces(tilo_server_protocol::workspaces::WorkspacesRequest::Name {
            workspace: workspace.clone(),
            name: "Ada".to_owned(),
        })
        .unwrap();
    assert!(matches!(
        tilo_test_env::within("the workspace names its person", named)
            .await
            .unwrap()
            .unwrap(),
        tilo_server_protocol::workspaces::WorkspacesReply::Named
    ));
    let thread = chat::Chat::direct_id([MemberId::person(person.clone()), MemberId::agent(SCOUT)]);
    let own = chat::Chat::direct_id([MemberId::person(person.clone()), MemberId::agent("writer")]);

    // What the sub-agent's turn calls next, one call for each step of a turn; and the one call
    // of the person's own agent, which posts into its thread once.
    let plan: Arc<Mutex<Vec<(String, serde_json::Value)>>> = Arc::new(Mutex::new(Vec::new()));
    let planned = Arc::clone(&plan);
    let posted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let post_once = Arc::clone(&posted);
    let main_thread = thread.as_str().to_owned();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = worked(
        &workspaces,
        &actions,
        plays(&asked, move |request| {
            let done = tool_answers(request).len();
            if of_sub_agent(request) {
                return match planned.lock().unwrap().get(done) {
                    Some((path, args)) => match path.strip_prefix(READ) {
                        Some(read) => reads(read),
                        None => does(done, path, args),
                    },
                    None => says("Tried."),
                };
            }
            if done == 0
                && said_in(request).contains("Post it.")
                && !post_once.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                return does(
                    0,
                    "chat/Messages#post",
                    &said(&main_thread, "Posted by the main agent.", None),
                );
            }
            says("Told.")
        }),
    );

    // The main thread holds something only it holds, and the person's own agent writes there
    // with the very call the sub-agent is refused below.
    let secret = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (thread.as_str(), "The launch's code word is heron. Post it."),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    reply_of(&workspaces, &workspace, &secret, SCOUT).await;
    let by_main = asked.lock().unwrap().last().map(tool_answers).unwrap();
    assert_eq!(by_main.len(), 1, "the person's own agent called once");
    assert!(
        !by_main[0].1,
        "and its post was carried out: {}",
        by_main[0].0
    );
    let before = said_there(&workspaces, &workspace, thread.as_str()).await;
    assert!(
        before.iter().any(|(_, message)| {
            message.author == MemberId::agent(SCOUT) && message.text == "Posted by the main agent."
        }),
        "the person's own agent writes into its thread"
    );
    let secret_before = format!(
        "{:?}",
        before
            .iter()
            .find(|(id, _)| *id == secret)
            .map(|(_, kept)| kept)
    );

    // Every write of Chat's, as Chat itself states them, aimed at the main thread and at the
    // message in it.
    let [answer, question, words] = ui::tilo_workspace_agent::sub::OWN_WRITES;
    let grows = ui::tilo_workspace_agent::sub::OWN_GROWTH;
    let of_messages = writes_of::<chat::parts::ChatPart>();
    let of_chat = writes_of::<chat::chat::Chat>();
    assert!(
        of_messages.len() >= 20
            && of_chat.len() >= 20
            && [
                answer,
                question,
                "post",
                "edit-message",
                "delete-message",
                "pin"
            ]
            .iter()
            .all(|verb| of_messages.iter().any(|stated| stated == verb)),
        "Chat states the actions that write: {of_messages:?} {of_chat:?}"
    );
    let mut elsewhere = said(thread.as_str(), "from below", None);
    elsewhere["message"] = serde_json::json!(secret.as_str());
    let mut attempts: Vec<(String, String, serde_json::Value)> = Vec::new();
    for verb in of_messages
        .iter()
        .filter(|verb| *verb != answer && *verb != question && *verb != words)
    {
        attempts.push((
            verb.clone(),
            format!("chat/Messages#{verb}"),
            elsewhere.clone(),
        ));
    }
    // The three it keeps are its own only where it works.
    attempts.push((
        question.to_owned(),
        format!("chat/Messages#{question}"),
        said(thread.as_str(), "May I write here?", None),
    ));
    attempts.push((
        answer.to_owned(),
        format!("chat/Messages#{answer}"),
        serde_json::json!({
            "message": secret.as_str(),
            "work": {"state": "taken"},
            "say": "from below",
        }),
    ));
    attempts.push((
        words.to_owned(),
        format!("chat/Messages#{words}"),
        said(thread.as_str(), "from below", None),
    ));
    for verb in &of_chat {
        attempts.push((verb.clone(), format!("chat#{verb}"), elsewhere.clone()));
    }
    for verb in REFUSED_BESIDE {
        attempts.push((verb.to_owned(), format!("chat#{verb}"), elsewhere.clone()));
    }
    // What its model writes while it writes goes into the reply its own turn answers, and
    // nowhere in the main thread: not into its starter's reply there, and not as a message
    // Chat would make of a reply it does not hold yet.
    let starters = reply_id(&secret, &MemberId::agent(SCOUT));
    for reply in [starters.as_str(), "m_reply_from_below"] {
        attempts.push((
            grows.to_owned(),
            format!("chat/Messages#{grows}"),
            grown(thread.as_str(), reply, "from below"),
        ));
    }

    // And once its starter was told how its job ended, which the server writes for it under
    // no rule of a turn, its next turn still reads nothing: the first call after its job.
    attempts.insert(
        12,
        (
            "read".to_owned(),
            format!("{READ}chat/Messages"),
            serde_json::Value::Null,
        ),
    );

    // A turn takes a bounded number of steps, so the calls are spread over several turns: its
    // job first, then each time its person writes to it.
    for (turn, calls) in attempts.chunks(12).enumerate() {
        *plan.lock().unwrap() = calls
            .iter()
            .map(|(_, path, args)| (path.clone(), args.clone()))
            .collect();
        let asked_with = if turn == 0 {
            starts(
                &workspaces,
                &workspace,
                &person,
                ("Writer", "Tidy the notes."),
            )
            .await;
            agents.look(&workspace).await.unwrap();
            caused(&workspaces, &workspace, (own.as_str(), "job"), 1).await[0]
                .0
                .clone()
        } else {
            writes_in(
                &actions,
                &workspaces,
                &workspace,
                &ada,
                (own.as_str(), &format!("Go on, part {turn}.")),
            )
            .await
        };
        agents.look(&workspace).await.unwrap();
        let answered = reply_of(&workspaces, &workspace, &asked_with, "writer").await;
        assert_eq!(answered.text, "Tried.", "its answer is its own to write");
        assert_eq!(*conversation_id(&answered), own);
        let answers = asked
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|request| of_sub_agent(request))
            .map(tool_answers)
            .unwrap();
        assert_eq!(
            answers.len(),
            calls.len(),
            "every call of the turn was answered"
        );
        for ((verb, path, _), (words, failed)) in calls.iter().zip(&answers) {
            if path.starts_with(READ) {
                assert!(
                    words.contains("not_permitted: not allowed") && !words.contains("heron"),
                    "after its starter was told, its read is still refused: {words}"
                );
                continue;
            }
            assert!(
                *failed && refused_in_words(words),
                "{verb} ({path}) is refused to a sub-agent: {words}"
            );
        }
    }

    // Nothing it called changed the main thread: the messages there are the ones that were
    // there, the one it aimed at is as it was, and the only message of its own is the news the
    // server wrote for it when its job ended.
    agents.look(&workspace).await.unwrap();
    let after = said_there(&workspaces, &workspace, thread.as_str()).await;
    for (id, message) in &after {
        let was_there = before.iter().any(|(held, _)| held == id);
        let news = message
            .cause
            .as_ref()
            .is_some_and(|cause| cause.kind.0 == "news");
        let reply_to_news = message.author == MemberId::agent(SCOUT);
        assert!(
            was_there || news || reply_to_news,
            "nothing a sub-agent called is in the main thread: {message:?}"
        );
        assert!(
            message.author != MemberId::agent("writer") || news,
            "a sub-agent wrote into the main thread: {message:?}"
        );
        assert!(!message.text.contains("from below"), "{message:?}");
    }
    assert_eq!(
        format!(
            "{:?}",
            after
                .iter()
                .find(|(id, _)| *id == secret)
                .map(|(_, kept)| kept)
        ),
        secret_before,
        "the message it aimed at is as it was"
    );

    // In its own conversation its words are plain words: each of these is refused there too.
    let with = |name: &str, value: serde_json::Value| {
        let mut posted = said(own.as_str(), "Look at this.", None);
        posted[name] = value;
        posted
    };
    let mut asked_under = said(own.as_str(), "May I?", None);
    asked_under["parent"] = serde_json::json!(secret.as_str());
    let kept_out: Vec<(&str, String, serde_json::Value)> = vec![
        (
            "a mention",
            format!("chat/Messages#{words}"),
            with(
                "mentions",
                serde_json::json!([{"start": 0, "len": 4, "name": "Scout", "is_agent": true}]),
            ),
        ),
        (
            "a member named in its words",
            format!("chat/Messages#{words}"),
            said(own.as_str(), "@Ada look at this.", None),
        ),
        (
            "a message it would stand under",
            format!("chat/Messages#{words}"),
            with("parent", serde_json::json!(secret.as_str())),
        ),
        (
            "something attached",
            format!("chat/Messages#{words}"),
            with("attachments", serde_json::json!([{"kind": "file"}])),
        ),
        (
            "a cause that asks another agent",
            "chat/Messages#ask-agent".to_owned(),
            said(
                own.as_str(),
                "Do more.",
                Some(serde_json::json!({"kind": "news", "label": "More", "key": "more-1"})),
            ),
        ),
        (
            "a question under another message",
            format!("chat/Messages#{question}"),
            asked_under,
        ),
        (
            "what its model writes, into a reply that is not its turn's",
            format!("chat/Messages#{grows}"),
            grown(own.as_str(), "m_reply_made_up", "Look at this."),
        ),
    ];
    let mut asked_own = said(own.as_str(), "Shall I also tidy the drafts?", None);
    asked_own["answers"] =
        serde_json::json!([{"id": "yes", "label": "Yes"}, {"id": "no", "label": "No"}]);
    let taken = [
        (
            format!("chat/Messages#{words}"),
            said(own.as_str(), "Half done: the notes are sorted.", None),
        ),
        (format!("chat/Messages#{question}"), asked_own),
    ];
    *plan.lock().unwrap() = kept_out
        .iter()
        .map(|(_, path, args)| (path.clone(), args.clone()))
        .chain(taken.iter().cloned())
        .collect();
    let last = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (own.as_str(), "Anything you need from me?"),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    reply_of(&workspaces, &workspace, &last, "writer").await;
    let in_own = asked
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|request| of_sub_agent(request))
        .map(tool_answers)
        .unwrap();
    assert_eq!(in_own.len(), kept_out.len() + taken.len());
    for ((what, path, _), (said, failed)) in kept_out.iter().zip(&in_own) {
        assert!(
            *failed && refused_in_words(said),
            "{what} ({path}) is refused in its own conversation too: {said}"
        );
    }
    for ((path, _), (said, failed)) in taken.iter().zip(&in_own[kept_out.len()..]) {
        assert!(!*failed, "{path} is taken where it works: {said}");
    }
    let there = said_there(&workspaces, &workspace, own.as_str()).await;
    let by_it = |text: &str| {
        there
            .iter()
            .find(|(_, message)| {
                message.author == MemberId::agent("writer") && message.text == text
            })
            .map(|(_, message)| message)
    };
    assert!(
        by_it("Half done: the notes are sorted.").is_some(),
        "its words stand in its own conversation"
    );
    for refused in ["Look at this.", "@Ada look at this.", "Do more.", "May I?"] {
        assert!(by_it(refused).is_none(), "{refused:?} was never written");
    }
    assert!(
        by_it("Shall I also tidy the drafts?").is_some_and(|message| message
            .question
            .as_ref()
            .is_some_and(chat::question::Question::waiting)),
        "its question waits in its own conversation"
    );
    stands_as(&workspaces, &workspace, "writer", Stands::NeedsPerson).await;
    page.peer.close();
}

/// A team from inside a turn: the person's own agent starts and steers its sub-agents from
/// its own turn, and a lead reads the team under it.
#[path = "hosted_sub_agent_teams.rs"]
mod teams;

/// Skills on the server: written and corrected from a turn, told to the turn they fit, and
/// what gives a sub-agent its model.
#[path = "hosted_skills.rs"]
mod skills;

/// What a look read before a turn ended, once that turn has ended.
#[path = "hosted_turn_ends.rs"]
mod turn_ends;

/// A reply grows while its model writes it.
#[path = "hosted_reply_grows.rs"]
mod reply_grows;

/// A model call its provider refuses for a moment is asked again, with the turn's work kept.
#[path = "hosted_turn_asks_again.rs"]
mod asks_again;
