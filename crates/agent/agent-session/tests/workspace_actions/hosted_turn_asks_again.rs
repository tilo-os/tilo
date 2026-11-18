//! A model call its provider refuses for a moment is asked again, and the turn goes on with
//! everything it did until then: no tool it carried out is carried out a second time. A
//! provider that stays down is asked for as long as it takes: no count of asks ends the turn,
//! its reply says each further ask as it happens, and its person stops it when they want. A
//! refusal that is the key's fault is the turn's answer at once.
use super::*;
use std::collections::VecDeque;
use tilo_server::workspaces::person_model::protocol::ErrorKind;

/// A model whose calls are written out, its provider's refusals among them: each ask takes the
/// next. What each request asked is kept.
struct Faltering {
    answers: Arc<Mutex<VecDeque<Result<ModelResponse, ModelError>>>>,
    asked: Arc<Mutex<Vec<ModelRequest>>>,
}

impl tilo_agent_session::model::PlanTurns for Faltering {
    fn turn(
        &mut self,
        request: &ModelRequest,
        _: &str,
        _: std::time::Instant,
        _: bool,
    ) -> Result<ModelResponse, ModelError> {
        self.asked.lock().unwrap().push(request.clone());
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(ModelError::invalid("nothing further is written")))
    }

    fn model(&self) -> &'static str {
        "faltering"
    }
}

/// A model that answers each ask with the next of `answers`.
fn faltering(
    answers: Vec<Result<ModelResponse, ModelError>>,
    asked: &Arc<Mutex<Vec<ModelRequest>>>,
) -> Model {
    let answers = Arc::new(Mutex::new(VecDeque::from(answers)));
    let asked = Arc::clone(asked);
    Arc::new(move || {
        Some(Box::new(Faltering {
            answers: Arc::clone(&answers),
            asked: Arc::clone(&asked),
        }) as Box<dyn tilo_agent_session::model::PlanTurns>)
    })
}

/// The model calls `tilo_do` with `input`, as its call `id`.
fn does(id: &str, input: serde_json::Value) -> ModelResponse {
    answer(
        vec![Content::ToolCall(ToolCall {
            id: id.into(),
            name: "tilo_do".into(),
            input,
            extra_content: None,
        })],
        StopReason::ToolCalls,
        1_000,
    )
}

/// The model answers in words.
fn says(text: &str) -> ModelResponse {
    answer(
        vec![Content::Text { text: text.into() }],
        StopReason::EndTurn,
        1_000,
    )
}

/// The provider refuses the call for now: what its status 503 reaches a turn as.
fn refused() -> ModelError {
    ModelError::new(ErrorKind::Provider)
}

/// The step that changes something: an event of the person's is set up.
fn sets_up() -> serde_json::Value {
    serde_json::json!({
        "path": "tilo.events/events#set-up",
        "args": {
            "name": "Call home",
            "when": "every day at 09:00",
            "instruction": "Say that it is time to call home.",
        },
    })
}

/// How many tools' answers `request` carries.
fn tools_answered(request: &ModelRequest) -> usize {
    request
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .filter(|content| matches!(content, Content::ToolResult(_)))
        .count()
}

const ASK: &str = "Remind me every morning to call home.";

/// The turn sets an event up, and the call after that step is refused once. The same call is
/// asked again and the turn goes on to its answer. The step before the refusal was carried
/// out once: the listing the model asks for next holds one event, not two.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refusal_between_two_steps_is_asked_again_and_no_step_is_done_twice() {
    team!(root, browser, actions, workspaces, workspace, page, person);
    let kept = crate::support::scratch_dir();
    let events = PersonEvents::start(workspaces.clone(), kept.path().join("events"), None);
    let first = write_to_agent(&actions, &workspaces, &workspace, (&root, &browser), ASK).await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = worked(
        &workspaces,
        &actions,
        faltering(
            vec![
                Ok(does("call-1", sets_up())),
                Err(refused()),
                Ok(does(
                    "call-2",
                    serde_json::json!({"path": "tilo.events/events#list"}),
                )),
                Ok(says("You will be reminded every morning.")),
            ],
            &asked,
        ),
    )
    .with_events(Some(events));
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    tokio::time::timeout(
        Duration::from_secs(60),
        agents.answer(&workspace, &turns[0]),
    )
    .await
    .expect("the turn ends")
    .unwrap();

    let reply = ended(&workspaces, &workspace, &first).await;
    let asked = asked.lock().unwrap().clone();
    assert_eq!(
        asked.len(),
        4,
        "the step, the call that was refused, the same call again, the answer; it said: {}",
        reply.text
    );
    assert_eq!(asked[1], asked[2], "the call is asked again as it was");
    let (set_up, failed) = answered_to(&asked[2], "call-1");
    assert!(!failed && set_up.starts_with("ok"), "{set_up}");
    assert_eq!(
        tools_answered(&asked[3]),
        2,
        "each of the two steps was carried out once"
    );
    let (listed, failed) = answered_to(&asked[3], "call-2");
    assert!(!failed && listed.starts_with("ok"), "{listed}");
    assert_eq!(
        listed.matches("Call home").count(),
        1,
        "the event was set up once, not once more for the call asked again: {listed}"
    );
    assert_eq!(reply.text, "You will be reminded every morning.");
    assert_eq!(
        end_of(&reply),
        Some(chat::agent_stream::AgentFinish::Complete)
    );
    let _ = person;
    page.peer.close();
}

/// The turn sets an event up, and then its provider is down for four asks and answers the
/// fifth. The turn goes on to its answer: no count of asks ended it. Each ask is the call as it
/// was, and the step before it is carried out once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_provider_down_for_four_asks_answers_the_fifth() {
    team!(root, browser, actions, workspaces, workspace, page, person);
    let kept = crate::support::scratch_dir();
    let events = PersonEvents::start(workspaces.clone(), kept.path().join("events"), None);
    let first = write_to_agent(&actions, &workspaces, &workspace, (&root, &browser), ASK).await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = worked(
        &workspaces,
        &actions,
        faltering(
            vec![
                Ok(does("call-1", sets_up())),
                Err(refused()),
                Err(refused()),
                Err(refused()),
                Err(refused()),
                Ok(says("You will be reminded every morning.")),
            ],
            &asked,
        ),
    )
    .with_events(Some(events));
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    tokio::time::timeout(
        Duration::from_secs(60),
        agents.answer(&workspace, &turns[0]),
    )
    .await
    .expect("the turn ends")
    .unwrap();

    let reply = ended(&workspaces, &workspace, &first).await;
    let asked = asked.lock().unwrap().clone();
    assert_eq!(
        asked.len(),
        6,
        "the step, then the call after it five times; it said: {}",
        reply.text
    );
    for again in &asked[2..] {
        assert_eq!(again, &asked[1], "each further ask is the call as it was");
        assert_eq!(tools_answered(again), 1, "the step was carried out once");
    }
    assert_eq!(reply.text, "You will be reminded every morning.");
    assert_eq!(
        end_of(&reply),
        Some(chat::agent_stream::AgentFinish::Complete)
    );
    let _ = person;
    page.peer.close();
}

/// The agent's reply to `message` as the workspace keeps it now, if it is there.
async fn reply_now(
    workspaces: &Workspaces,
    workspace: &str,
    message: &Id,
) -> Option<chat::chat::Message> {
    conversation_part(workspaces, workspace)
        .await
        .messages
        .get(&chat::chat::reply_id(message, &MemberId::agent(SCOUT)))
        .cloned()
}

/// What `reply`'s work says the turn is doing now, if it says so.
fn doing(reply: &chat::chat::Message) -> Option<String> {
    match &reply.work.as_ref()?.state.as_ref()?.state {
        chat::model::AgentWorkState::Doing { did, .. } => Some(did.clone()),
        _ => None,
    }
}

/// The turn sets an event up, and then its provider stays down. The call is asked a fifth time
/// and would be asked on: nothing of the server's ends the turn. Its reply says which ask the
/// call is at and the provider's last word, so its person sees the wait. They stop the turn,
/// and it ends there, with no ask after the stop.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_provider_that_stays_down_is_asked_until_its_person_stops_the_turn() {
    team!(root, browser, actions, workspaces, workspace, page, person);
    let me = themselves(&root, &browser, &workspace);
    let kept = crate::support::scratch_dir();
    let events = PersonEvents::start(workspaces.clone(), kept.path().join("events"), None);
    let first = write_to_agent(&actions, &workspaces, &workspace, (&root, &browser), ASK).await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let mut answers = vec![Ok(does("call-1", sets_up()))];
    answers.extend((0..40).map(|_| Err(refused())));
    let agents =
        worked(&workspaces, &actions, faltering(answers, &asked)).with_events(Some(events));
    agents.look(&workspace).await.unwrap();

    // The step, then the call after it: a fifth ask of it, where four used to be all.
    let deadline = tilo_test_env::hang_deadline();
    while asked.lock().unwrap().len() < 6 {
        let reply = reply_now(&workspaces, &workspace, &first).await;
        assert!(
            chat::summons::unanswered(reply.as_ref()),
            "the turn ended by itself after {} asks of its model, and nobody stopped it: {:?}",
            asked.lock().unwrap().len(),
            reply.map(|reply| reply.text)
        );
        assert!(Instant::now() < deadline, "the call is asked a fifth time");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    // While it waits, the reply says so: which ask this is, and the provider's last word.
    let said = "asking my model again (ask 5): Provider request failed";
    loop {
        let reply = reply_now(&workspaces, &workspace, &first).await;
        let says = reply.as_ref().and_then(doing);
        if says.as_deref() == Some(said) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the reply never said that its model is asked again; its work says: {says:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    stop_agent(&actions, &workspace, &me, &first)
        .await
        .expect("the person stops their agent's work");
    agents.look(&workspace).await.unwrap();
    let reply = ended(&workspaces, &workspace, &first).await;
    assert_eq!(
        end_of(&reply),
        Some(chat::agent_stream::AgentFinish::Cancelled),
        "the stop ended the turn: {:?}",
        reply.work
    );
    let asked = asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 6, "no ask follows the stop");
    for again in &asked[2..] {
        assert_eq!(again, &asked[1], "each further ask is the call as it was");
        assert_eq!(tools_answered(again), 1, "the step was carried out once");
    }
    let _ = person;
    page.peer.close();
}

/// A provider that refuses the key is not asked again: the turn's person is told at once, in
/// the provider's own failure and nothing after it, since the turn had done nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refusal_that_is_the_keys_fault_is_not_asked_again() {
    team!(root, browser, actions, workspaces, workspace, page, person);
    let first = write_to_agent(&actions, &workspaces, &workspace, (&root, &browser), ASK).await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = worked(
        &workspaces,
        &actions,
        faltering(
            vec![
                Err(ModelError::new(ErrorKind::Authentication)),
                Ok(says("This is never said.")),
            ],
            &asked,
        ),
    );
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    tokio::time::timeout(
        Duration::from_secs(60),
        agents.answer(&workspace, &turns[0]),
    )
    .await
    .expect("the turn ends")
    .unwrap();

    let reply = ended(&workspaces, &workspace, &first).await;
    assert_eq!(asked.lock().unwrap().len(), 1, "asked once: {}", reply.text);
    assert_eq!(
        reply.text,
        "My model did not answer: Provider authentication failed; check the credential in Settings"
    );
    assert_eq!(
        end_of(&reply),
        Some(chat::agent_stream::AgentFinish::Failed)
    );
    let _ = person;
    page.peer.close();
}
