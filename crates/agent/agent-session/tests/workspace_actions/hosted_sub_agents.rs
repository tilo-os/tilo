//! Sub-agents on the server, with no device of the person running anything: the person's own
//! agent starts one, it works in a conversation of its own, a note reaches it at its next step,
//! several work at once, its person writes into one and stops one, and when work its starter
//! gave it ends, its starter is told. And the one-way rule as a permission: what a sub-agent's
//! own turn is answered when it reads the thread of the agent that started it.
use super::*;
use chat::chat::{Message as Kept, reply_id};
use tilo_relay::workspaces::Workspaces;
use ui::tilo_resource::{Invoke, NonBlank, Replica};
use ui::tilo_workspace_agent::{Agent, NewSubAgent, Stands, workspace_agents};

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
    // The same read the person's own agent was answered is not offered to the sub-agent at all.
    let refused = tools_said(&subs[1]);
    assert_ne!(
        refused, main_read,
        "it is not answered what its starter was"
    );
    assert!(
        refused.contains("chat/Messages has no read action"),
        "its read of the chat is not offered to it: {refused}"
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
    let beside = ada.clone().delegated_to(id.clone()).named("Find the deck");
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
