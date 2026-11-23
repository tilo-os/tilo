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

/// The person of `person` (their root, hex) stops the turn their message `message` asked for:
/// the agent package's `stop` on the ask the message made.
async fn stop_agent(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    person: &str,
    message: &Id,
) -> Result<(), String> {
    let packages = asks::chat_packages().unwrap();
    let who = asks::Actor::Member(MemberId::person(person.to_owned()));
    let agent = MemberId::agent(SCOUT);
    let message = message.clone();
    workspaces
        .with_tl_record(workspace, &packages, who.clone(), move |opened| {
            let ask = asks::asks(opened)?
                .into_iter()
                .find(|ask| {
                    ask.agent == agent
                        && ask
                            .from
                            .as_ref()
                            .is_some_and(|from| from.id_string() == message.as_str())
                })
                .ok_or_else(|| format!("no ask stands under {message}"))?;
            opened.invoke(
                &who,
                (asks::AGENT_APP, "stop"),
                vec![TlValue::resource(&ask.at)],
                None,
            )?;
            Ok(())
        })
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

/// A turn's answer, as the ask's newest reply holds it.
#[derive(Clone, Debug)]
pub(super) struct Answer {
    pub text: String,
    pub state: asks::AskState,
    /// What it called on the way, in order.
    pub steps: Vec<String>,
}

impl Answer {
    fn of(reply: &asks::Replied) -> Self {
        Self {
            text: reply.body.clone(),
            state: reply.state,
            steps: reply.steps.iter().map(|step| step.called.clone()).collect(),
        }
    }
}

/// How a turn ended, as its reply's state says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AgentFinish {
    Complete,
    Failed,
    Cancelled,
}

/// The agent's answer to `message`, once its turn ended.
async fn ended(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    message: &Id,
) -> Answer {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let asked_now = asked_on(workspaces, workspace).await;
        let answer = ask_on(&asked_now, message, SCOUT)
            .and_then(|ask| ask.latest())
            .map(Answer::of);
        if let Some(answer) = &answer
            && !matches!(
                answer.state,
                asks::AskState::Waiting | asks::AskState::Working
            )
        {
            return answer.clone();
        }
        assert!(
            Instant::now() < deadline,
            "the agent's reply to {message} never ended: {answer:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// How the work a reply records ended.
fn end_of(answer: &Answer) -> Option<AgentFinish> {
    match answer.state {
        asks::AskState::Done => Some(AgentFinish::Complete),
        asks::AskState::Failed => Some(AgentFinish::Failed),
        asks::AskState::Stopped => Some(AgentFinish::Cancelled),
        _ => None,
    }
}

/// Whether the agent's turn on `message` has begun: it took the ask and wrote at least one
/// step, and no end is written.
pub(super) fn left_begun(asked: &[asks::Asked], message: &Id, agent: &str) -> bool {
    ask_on(asked, message, agent).is_some_and(|ask| {
        ask.state == asks::AskState::Working
            && ask.latest().is_some_and(|reply| !reply.steps.is_empty())
    })
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
async fn a_persons_stop_reaches_a_turn_that_is_going() {
    use tilo_server::workspaces::person_model::{
        ProviderProtocol,
        scripted::{Linking, Reply, ScriptedProvider},
    };

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
    stop_agent(&workspaces, &workspace, &person, &first)
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
        Some(AgentFinish::Cancelled),
        "its reply says it was stopped: {:?}",
        reply.state
    );
    assert_eq!(asked.lock().unwrap().len(), 2, "no step follows a stop");
    assert!(agents.turns(&workspace).await.unwrap().is_empty());

    // A call of the person's own model that is under way ends on the stop: this provider never
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
                key: Some("sk-the-persons-own"),
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
    stop_agent(&workspaces, &workspace, &person, &second)
        .await
        .expect("the person stops their agent's work");
    agents.look(&workspace).await.unwrap();
    let reply = ended(&workspaces, &workspace, &second).await;
    assert_eq!(
        end_of(&reply),
        Some(AgentFinish::Cancelled),
        "the stop ended the call under way: {:?}",
        reply.state
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
    let asked_now = loop {
        let asked_now = asked_on(&workspaces, &workspace).await;
        if left_begun(&asked_now, &first, SCOUT) || Instant::now() > deadline {
            break asked_now;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    assert!(
        left_begun(&asked_now, &first, SCOUT),
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
        Some(AgentFinish::Failed),
        "so the person can ask again: {:?}",
        reply.state
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
    let asked_now = asked_on(&workspaces, &workspace).await;
    assert!(
        ask_on(&asked_now, &second, SCOUT).is_some_and(|ask| {
            matches!(ask.state, asks::AskState::Waiting | asks::AskState::Working)
        }),
        "the turn was taken and has not ended"
    );
    assert!(
        !left_begun(&asked_now, &second, SCOUT),
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
    assert!(
        reply.text.contains("interrupted"),
        "its reply says what happened: {}",
        reply.text
    );
    assert_eq!(
        end_of(&reply),
        Some(AgentFinish::Failed),
        "so the person can ask again: {:?}",
        reply.state
    );
    assert!(
        again.lock().unwrap().is_empty(),
        "no model is asked and no step is taken for it"
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
mod sub_agents;

/// A reminder a person's assistant set up itself, from the asking to the answer.
#[path = "hosted_agent_reminders.rs"]
mod reminders;
