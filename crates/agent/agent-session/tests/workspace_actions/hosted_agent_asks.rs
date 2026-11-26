//! What starts a turn of an agent the server works besides its person's typing: a cause that asks
//! through Chat's `ask-agent`. And what reaches a turn that is going: what its person writes
//! next, their stop, and the server's own restart. All through its own interfaces, with
//! no device of the person running anything.
use super::*;
use std::sync::Condvar;
use tilo_actions::remote::Caller;

/// A hosted workspace with one person in its chat and their agent made to work on the server,
/// as the tests beside this one set it up. The person's page joins: its device is the one the
/// server admits their agent's turns on. No agent works on that device.
macro_rules! team {
    ($root:ident, $browser:ident, $actions:ident, $workspaces:ident, $workspace:ident, $page:ident, $person:ident, $served:ident) => {
        team!(
            $root,
            $browser,
            $actions,
            $workspaces,
            $workspace,
            $page,
            $person,
            $served,
            _device
        );
    };
    // The same, for a test in which another person's page joins as well: it names what serves
    // the workspace and the device that serves it.
    ($root:ident, $browser:ident, $actions:ident, $workspaces:ident, $workspace:ident, $page:ident, $person:ident, $served:ident, $device:ident) => {
        let ($root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
        let $device = enrol(&$root, "tilo-sh", DeviceCapabilities::member());
        let $browser = enrol(&$root, "browser", DeviceCapabilities::member());
        let state = crate::support::scratch_dir();
        let $served = crate::harness::hosted::start(
            &$root,
            &$device,
            state.path(),
            crate::harness::hosted::Options::offering(crate::common::catalogue),
        )
        .await;
        #[allow(
            unused_variables,
            reason = "not every test acts through the server's actions"
        )]
        let $actions = $served.roster.actions.clone();
        let $workspaces = $served.workspaces().unwrap().clone();
        let $workspace = $workspaces.create("Team").await.unwrap().workspace;
        let $page = Browser::open_at(&$served, &$browser, &$device, &$workspace).await;
        let $person = hex::encode($root.verifying_key().to_bytes());
        make_agent(&$page, &$workspaces, &$workspace, &$person, None).await;
        let catalogue = crate::common::catalogue();
        let governor = $workspaces.governor().clone();
        $workspaces
            .prepare_application(&$workspace, &governor, &catalogue, CHAT, "")
            .await
            .unwrap();
    };
}

/// The person, at their page's device.
fn themselves(root: &SigningKey, browser: &Device, workspace: &str) -> Caller {
    Caller::authenticated(
        root.verifying_key().to_bytes(),
        browser.cert.device_public_key,
    )
    .in_workspace(workspace)
}

/// Invoke Chat's `action` on `resource` of `record` as `caller`, through the server's action
/// service.
async fn chat_action(
    actions: &tilo_agent_session::workspace_actions::WorkspaceActions,
    workspace: &str,
    caller: &Caller,
    (record, resource): (&str, &str),
    action: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value, String> {
    use tilo_workspace_service::agents;
    let provider = agents::WorkspaceActionProvider::new(
        actions.clone(),
        WorkspaceId::parse(workspace).unwrap(),
    );
    let route = DocumentRoute {
        app_id: CHAT.into(),
        cluster_id: workspace.into(),
        document: record.into(),
    };
    agents::invoke_named(
        &provider,
        caller,
        &route,
        resource,
        action,
        arguments.as_object().unwrap().clone(),
    )
    .await
}

/// `caller` asks the agents of `conversation` through Chat's `ask-agent`, as a reminder under
/// `key`.
async fn ask_agent(
    actions: &tilo_agent_session::workspace_actions::WorkspaceActions,
    workspace: &str,
    caller: &Caller,
    conversation: &Id,
    key: &str,
) -> Result<serde_json::Value, String> {
    chat_action(
        actions,
        workspace,
        caller,
        ("", &ui::resource::chain_resource_id(&chat::parts::chain())),
        "ask-agent",
        serde_json::json!({
            "channel": conversation.as_str(),
            "text": "Call mum about Sunday",
            "cause": {"kind": "reminder", "label": "Call mum", "key": key},
        }),
    )
    .await
}

/// `caller` stops the turn `message` asks for, through Chat's `stop-agent`.
async fn stop_agent(
    actions: &tilo_agent_session::workspace_actions::WorkspaceActions,
    workspace: &str,
    caller: &Caller,
    message: &Id,
) -> Result<serde_json::Value, String> {
    chat_action(
        actions,
        workspace,
        caller,
        (
            chat::parts::part(0).as_str(),
            &format!("{}/messages/{message}", chat::parts::ChatMessages::KIND),
        ),
        "stop-agent",
        serde_json::json!({"message": message.as_str()}),
    )
    .await
}

/// A hold a scripted model waits at until the test opens it. It opens when it is dropped, so a
/// failed test never leaves a model's thread waiting.
#[derive(Default)]
struct Hold {
    open: Mutex<bool>,
    opened: Condvar,
}

impl Hold {
    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.opened.notify_all();
    }

    fn pass(&self) {
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.opened.wait(open).unwrap();
        }
    }
}

/// Opens its hold when the test ends, however it ends.
struct Opens(Arc<Hold>);

impl Drop for Opens {
    fn drop(&mut self) {
        self.0.open();
    }
}

/// A model whose answers are written out, in order over every turn it is made for (a server makes
/// its model anew for each turn), the last one again once they are used up. It waits at its hold
/// before it answers the call numbered `hold_at`, counted from 0 over every call it is asked.
struct Waits {
    answers: Arc<Mutex<Vec<ModelResponse>>>,
    asked: Arc<Mutex<Vec<ModelRequest>>>,
    hold_at: usize,
    hold: Arc<Hold>,
}

impl tilo_agent_session::model::PlanTurns for Waits {
    fn turn(
        &mut self,
        request: &ModelRequest,
        _: &str,
        _: std::time::Instant,
        _: bool,
    ) -> Result<ModelResponse, ModelError> {
        let index = {
            let mut asked = self.asked.lock().unwrap();
            asked.push(request.clone());
            asked.len() - 1
        };
        if index == self.hold_at {
            self.hold.pass();
        }
        let mut answers = self.answers.lock().unwrap();
        Ok(if answers.len() > 1 {
            answers.remove(0)
        } else {
            answers[0].clone()
        })
    }

    fn model(&self) -> &'static str {
        "waits"
    }
}

fn waits(
    answers: Vec<ModelResponse>,
    hold_at: usize,
    asked: &Arc<Mutex<Vec<ModelRequest>>>,
    hold: &Arc<Hold>,
) -> Model {
    let (asked, hold) = (Arc::clone(asked), Arc::clone(hold));
    let answers = Arc::new(Mutex::new(answers));
    Arc::new(move || {
        Some(Box::new(Waits {
            answers: Arc::clone(&answers),
            asked: Arc::clone(&asked),
            hold_at,
            hold: Arc::clone(&hold),
        }) as Box<dyn tilo_agent_session::model::PlanTurns>)
    })
}

/// A model turn that looks through the workspace.
fn looks() -> ModelResponse {
    answer(
        vec![Content::ToolCall(ToolCall {
            id: "call-1".into(),
            name: "tilo_ls".into(),
            input: serde_json::json!({}),
            extra_content: None,
        })],
        StopReason::ToolCalls,
        1_000,
    )
}

/// A model turn that answers `text`.
fn says(text: &str) -> ModelResponse {
    answer(
        vec![Content::Text { text: text.into() }],
        StopReason::EndTurn,
        1_000,
    )
}

/// Wait until `ready`.
async fn until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = tilo_test_env::hang_deadline();
    while !ready() {
        assert!(Instant::now() < deadline, "{what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// The agent's reply to `message`, once its work ended.
async fn ended(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    message: &Id,
) -> chat::chat::Message {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let part = conversation_part(workspaces, workspace).await;
        let reply = part
            .messages
            .get(&chat::chat::reply_id(message, &MemberId::agent(SCOUT)));
        if let Some(reply) = reply
            && !chat::summons::unanswered(Some(reply))
        {
            return reply.clone();
        }
        assert!(
            Instant::now() < deadline,
            "the agent's reply to {message} never ended: {:?}",
            reply.map(|reply| reply.work.clone())
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// How the work a reply records ended.
fn end_of(reply: &chat::chat::Message) -> Option<chat::agent_stream::AgentFinish> {
    match reply.work.as_ref()?.state.as_ref()?.state {
        chat::model::AgentWorkState::ProviderFinished { outcome } => Some(outcome),
        _ => None,
    }
}

/// Run `turn` on `agents` until its model was asked `calls` times, then let the server go: the
/// turn is dropped where it stands, as when the server's process ends.
async fn dies_at(
    agents: &ServerAgents,
    workspace: &str,
    turn: &tilo_workspace_service::agents::Turn,
    asked: &Arc<Mutex<Vec<ModelRequest>>>,
    calls: usize,
) {
    let working = agents.answer(workspace, turn);
    tokio::pin!(working);
    let deadline = tilo_test_env::hang_deadline();
    while asked.lock().unwrap().len() < calls {
        assert!(
            Instant::now() < deadline,
            "the turn never reached its model's call {calls}"
        );
        tokio::select! {
            ended = &mut working => panic!("the turn ended before the server stopped: {ended:?}"),
            () = tokio::time::sleep(Duration::from_millis(25)) => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cause_asks_the_agent_on_the_host_once_for_the_person_it_names() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let thread = chat::Chat::direct_id([MemberId::person(person.clone()), MemberId::agent(SCOUT)]);
    // What asks here: another agent of the person's, acting for them, as a sub-agent with news
    // or the runner of their reminder does. The person is never an argument: the server says who
    // it acts for.
    let planner = themselves(&root, &browser, &workspace)
        .delegated_to("planner")
        .named("Planner");

    // Where nobody could answer, nothing is asked: a conversation no agent takes part in, and
    // the conversation's own agent asking itself.
    let alone = chat::Chat::direct_id([MemberId::person(person.clone())]);
    assert!(
        ask_agent(&actions, &workspace, &planner, &alone, "run-0")
            .await
            .is_err(),
        "no agent takes part there"
    );
    let scout = themselves(&root, &browser, &workspace)
        .delegated_to(SCOUT)
        .named("Scout");
    assert!(
        ask_agent(&actions, &workspace, &scout, &thread, "run-0")
            .await
            .is_err(),
        "an agent does not start its own turn"
    );

    // The reminder asks, and its runner asks again (its run was taken again): one message.
    ask_agent(&actions, &workspace, &planner, &thread, "run-1")
        .await
        .expect("what acts for the person asks their agent");
    ask_agent(&actions, &workspace, &planner, &thread, "run-1")
        .await
        .expect("asking again under its key is no failure");
    let id = chat::cause::message_id(&MemberId::person(person.clone()), "run-1");
    let part = conversation_part(&workspaces, &workspace).await;
    assert_eq!(
        part.messages.len(),
        1,
        "one key is one message, and what was refused wrote none: {:?}",
        part.messages
            .iter()
            .map(|(_, held)| (held.author.clone(), held.text.clone()))
            .collect::<Vec<_>>()
    );
    let message = part
        .messages
        .get(&id)
        .expect("the message stands at the id its person and key derive");
    let cause = message.cause.as_ref().expect("it carries its cause");
    assert_eq!(cause.person, MemberId::person(person.clone()));
    assert_eq!(
        (cause.kind.0.as_str(), cause.label.as_str()),
        ("reminder", "Call mum"),
        "it says what it is, for the thread to show"
    );
    assert_eq!(
        message.author,
        MemberId::agent("planner"),
        "what asked wrote it, never the person"
    );

    // With no device on, the server reads it as the agent's one turn, for that person.
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = served
        .approving(&workspace, &[SCOUT])
        .await
        .scripted(looks_then_answers(1_000, Arc::clone(&asked)));
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "the ask is the agent's one turn: {turns:?}");
    assert_eq!(
        turns[0].person, person,
        "it works for the person the cause names"
    );
    assert_eq!(turns[0].cause, id.to_string());
    assert!(
        turns[0]
            .request
            .starts_with("A reminder your person set is due now: \"Call mum\".")
            && turns[0].request.ends_with("Call mum about Sunday"),
        "the agent is told what started the turn, then the instruction: {}",
        turns[0].request
    );
    tokio::time::timeout(
        Duration::from_secs(30),
        agents.answer(&workspace, &turns[0]),
    )
    .await
    .expect("the turn ends")
    .unwrap();

    // The answer is in the conversation: the agent's reply to the ask, where whoever started
    // the turn reads how it ended.
    let reply = ended(&workspaces, &workspace, &id).await;
    assert!(reply.text.contains("the plan is ready"), "{}", reply.text);
    assert_eq!(reply.author, MemberId::agent(SCOUT), "the agent wrote it");
    assert_eq!(
        asked.lock().unwrap().len(),
        2,
        "one turn to look, one to answer"
    );
    assert!(
        agents.turns(&workspace).await.unwrap().is_empty(),
        "answered once"
    );

    // Asked once more after it was answered (the runner started again): nothing starts.
    ask_agent(&actions, &workspace, &planner, &thread, "run-1")
        .await
        .expect("asking again is no failure");
    assert!(agents.turns(&workspace).await.unwrap().is_empty());
    assert_eq!(
        conversation_part(&workspaces, &workspace)
            .await
            .messages
            .len(),
        2,
        "the ask and its answer, nothing else"
    );
    page.peer.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_its_person_writes_reaches_a_going_turn_and_what_a_cause_asks_waits_for_it() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let thread = chat::Chat::direct_id([MemberId::person(person.clone()), MemberId::agent(SCOUT)]);
    let written = (&root, &browser);
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        written,
        "plan the launch",
    )
    .await;
    let (asked, hold) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Hold::default()));
    let _opens = Opens(Arc::clone(&hold));
    let agents = served.approving(&workspace, &[SCOUT]).await.scripted(waits(
        vec![
            looks(),
            says("The launch is planned."),
            says("The launch is planned and the room is booked."),
            says("It is time to call mum."),
        ],
        1,
        &asked,
        &hold,
    ));
    agents.look(&workspace).await.unwrap();
    until("the turn reaches its second step", || {
        asked.lock().unwrap().len() == 2
    })
    .await;

    // While it works a reminder comes due, and then the person writes again.
    let planner = themselves(&root, &browser, &workspace)
        .delegated_to("planner")
        .named("Planner");
    ask_agent(&actions, &workspace, &planner, &thread, "run-1")
        .await
        .expect("the reminder asks");
    agents.look(&workspace).await.unwrap();
    let second = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        written,
        "also book the room",
    )
    .await;
    agents.look(&workspace).await.unwrap();
    agents.look(&workspace).await.unwrap();
    hold.open();

    // The turn goes on under what the person wrote next: there its answer stands, and the reply
    // to their first message says so. No second answer was started beside it.
    let answer = ended(&workspaces, &workspace, &second).await;
    assert_eq!(answer.text, "The launch is planned and the room is booked.");
    let left = ended(&workspaces, &workspace, &first).await;
    assert!(
        left.text.contains("going on with what you wrote next"),
        "the reply to the first message says where the answer is: {}",
        left.text
    );

    // The reminder waited for the turn to end, and is then answered in a turn of its own.
    let reminder = chat::cause::message_id(&MemberId::person(person.clone()), "run-1");
    let reminded = ended(&workspaces, &workspace, &reminder).await;
    assert_eq!(reminded.text, "It is time to call mum.");
    until("every turn ended", || asked.lock().unwrap().len() == 4).await;
    let requests = asked.lock().unwrap().clone();
    assert_eq!(
        requests.len(),
        4,
        "three calls for the person's turn, one for the reminder's"
    );
    let told = format!("{:?}", requests[2].messages);
    assert!(
        told.contains("plan the launch")
            && told.contains("your person wrote again")
            && told.contains("also book the room"),
        "at its next step the turn is told what its person wrote since: {told}"
    );
    let apart = format!("{:?}", requests[3].messages);
    assert!(
        requests[3]
            .messages
            .iter()
            .flat_map(|message| message.content.iter())
            .all(|content| matches!(content, Content::Text { .. }))
            && !apart.contains("your person wrote again"),
        "the reminder is a turn of its own, asked afresh: no step of the person's turn goes on in it: {apart}"
    );
    assert!(
        asked_in(&requests[3]).contains("A reminder your person set is due now"),
        "and it is told as what it is: {apart}"
    );
    let part = conversation_part(&workspaces, &workspace).await;
    let replies = part
        .messages
        .values()
        .filter(|held| held.author == MemberId::agent(SCOUT))
        .count();
    assert_eq!(replies, 3, "one reply to each ask, none beside them");
    let deadline = tilo_test_env::hang_deadline();
    while !agents.turns(&workspace).await.unwrap().is_empty() {
        assert!(Instant::now() < deadline, "every ask is answered");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    page.peer.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_persons_stop_reaches_a_turn_that_is_going() {
    use tilo_server::workspaces::person_model::{
        ProviderProtocol,
        scripted::{Linking, Reply, ScriptedProvider},
    };

    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let written = (&root, &browser);
    let me = themselves(&root, &browser, &workspace);
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        written,
        "plan the launch",
    )
    .await;
    let (asked, hold) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Hold::default()));
    let _opens = Opens(Arc::clone(&hold));
    let agents = served.approving(&workspace, &[SCOUT]).await.scripted(waits(
        vec![looks(), says("The launch is planned.")],
        1,
        &asked,
        &hold,
    ));
    agents.look(&workspace).await.unwrap();
    until("the turn reaches its second step", || {
        asked.lock().unwrap().len() == 2
    })
    .await;

    // The person stops it: the message asks nobody any more, and the turn ends at its next
    // step. What its model said after the stop is not posted, and no step follows.
    stop_agent(&actions, &workspace, &me, &first)
        .await
        .expect("the person stops their agent's work");
    assert!(
        agents.turns(&workspace).await.unwrap().is_empty(),
        "a stopped message asks nobody"
    );
    agents.look(&workspace).await.unwrap();
    hold.open();
    let reply = ended(&workspaces, &workspace, &first).await;
    assert_eq!(
        end_of(&reply),
        Some(chat::agent_stream::AgentFinish::Cancelled),
        "its reply says it was stopped: {:?}",
        reply.work
    );
    assert_eq!(reply.text, "", "nothing it said after the stop is posted");
    assert_eq!(asked.lock().unwrap().len(), 2, "no step follows a stop");
    assert!(agents.turns(&workspace).await.unwrap().is_empty());

    // A call of the keeper's own model that is under way ends on the stop: this provider never
    // answers, so only the stop ends the turn.
    let theirs = ScriptedProvider::answering([Reply::never()]);
    workspaces.answer_models_with(theirs.clone());
    workspaces
        .link_model_for(
            &workspace,
            &Linking {
                name: "Google",
                protocol: ProviderProtocol::OpenAiCompatible,
                endpoint: "https://generativelanguage.googleapis.com/v1beta/openai",
                key: Some("sk-the-keepers-own"),
                models: &["gemini-main"],
            },
            true,
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let second = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        written,
        "and the budget?",
    )
    .await;
    let agents = served.approving(&workspace, &[SCOUT]).await;
    agents.look(&workspace).await.unwrap();
    until("the keeper's model is asked", || theirs.asked().len() == 1).await;
    stop_agent(&actions, &workspace, &me, &second)
        .await
        .expect("the person stops their agent's work");
    agents.look(&workspace).await.unwrap();
    let reply = ended(&workspaces, &workspace, &second).await;
    assert_eq!(
        end_of(&reply),
        Some(chat::agent_stream::AgentFinish::Cancelled),
        "the stop ended the call under way: {:?}",
        reply.work
    );
    assert_eq!(theirs.asked().len(), 1, "it was not asked again");
    page.peer.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn after_a_restart_a_turn_left_begun_is_not_started_over_and_one_only_taken_runs_once() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let written = (&root, &browser);
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        written,
        "plan the launch",
    )
    .await;

    // The server stops while the turn works: it had looked through the workspace, and its model
    // was asked what to do next.
    let (asked, hold) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Hold::default()));
    let _opens = Opens(Arc::clone(&hold));
    let dying = served.approving(&workspace, &[SCOUT]).await.scripted(waits(
        vec![looks(), says("never said")],
        1,
        &asked,
        &hold,
    ));
    let turns = dying.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    dies_at(&dying, &workspace, &turns[0], &asked, 2).await;
    drop(dying);
    // The note of a look reaches the reply once the look has answered, and no tool waits for
    // it: the reply shows the step a moment after its model was asked.
    let deadline = tilo_test_env::hang_deadline();
    let part = loop {
        let part = conversation_part(&workspaces, &workspace).await;
        if chat::summons::left_begun(&part, first.as_str(), SCOUT, 0) || Instant::now() > deadline {
            break part;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    assert!(
        chat::summons::left_begun(&part, first.as_str(), SCOUT, 0),
        "its record shows a step taken and no end"
    );

    // The server that starts again reads the turn from the workspace. It does not take its steps a
    // second time: the reply says it was interrupted, and asking again is the person's to do.
    let again = Arc::new(Mutex::new(Vec::new()));
    let restarted = served
        .approving(&workspace, &[SCOUT])
        .await
        .scripted(looks_then_answers(1_000, Arc::clone(&again)));
    let turns = restarted.turns(&workspace).await.unwrap();
    assert_eq!(
        turns.len(),
        1,
        "the unfinished turn is still owed: {turns:?}"
    );
    restarted.answer(&workspace, &turns[0]).await.unwrap();
    let reply = ended(&workspaces, &workspace, &first).await;
    assert!(
        reply.text.contains("interrupted"),
        "its reply says what happened: {}",
        reply.text
    );
    assert_eq!(
        end_of(&reply),
        Some(chat::agent_stream::AgentFinish::Failed),
        "so the person can ask again: {:?}",
        reply.work
    );
    assert!(
        again.lock().unwrap().is_empty(),
        "no model is asked and no step is taken a second time"
    );
    assert!(restarted.turns(&workspace).await.unwrap().is_empty());
    drop(restarted);

    // A turn the server had only taken changed nothing yet: after the restart it runs, once.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let second = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        written,
        "and the budget?",
    )
    .await;
    let (waiting, second_hold) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Hold::default()));
    let _opens_second = Opens(Arc::clone(&second_hold));
    let dying = served.approving(&workspace, &[SCOUT]).await.scripted(waits(
        vec![says("never said")],
        0,
        &waiting,
        &second_hold,
    ));
    let turns = dying.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    dies_at(&dying, &workspace, &turns[0], &waiting, 1).await;
    drop(dying);
    let part = conversation_part(&workspaces, &workspace).await;
    assert!(
        part.messages
            .get(&chat::chat::reply_id(&second, &MemberId::agent(SCOUT)))
            .is_some_and(|reply| chat::summons::unanswered(Some(reply))),
        "the turn was taken and has not ended"
    );
    assert!(
        !chat::summons::left_begun(&part, second.as_str(), SCOUT, 0),
        "it had taken no step"
    );
    let restarted = served
        .approving(&workspace, &[SCOUT])
        .await
        .scripted(looks_then_answers(1_000, Arc::clone(&again)));
    let turns = restarted.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    restarted.answer(&workspace, &turns[0]).await.unwrap();
    let reply = ended(&workspaces, &workspace, &second).await;
    assert!(reply.text.contains("the plan is ready"), "{}", reply.text);
    assert_eq!(
        again.lock().unwrap().len(),
        2,
        "it ran once: one call to look, one to answer"
    );
    assert!(
        restarted.turns(&workspace).await.unwrap().is_empty(),
        "and it is not run again"
    );
    let _ = person;
    page.peer.close();
}

/// Sub-agents: started by the person's own agent, working in conversations of their own.
#[path = "hosted_sub_agents.rs"]
pub(super) mod sub_agents;

/// A reminder a person's assistant set up itself, from the asking to the answer.
#[path = "hosted_agent_reminders.rs"]
mod reminders;
