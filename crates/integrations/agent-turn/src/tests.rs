//! A turn whose tools the device's session answers over a carried link: the model's calls go to
//! that session as its agent session's steps, its answer ends the turn, a stop ends it, and a long
//! turn keeps within its bound.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};

use agent_provider_config::protocol::Usage;
use serde_json::json;
use tilo_action_services::TestProvider;
use tilo_action_services::agent_tools as wire;
use tilo_actions::ActionOutput;
use tilo_actions::remote::{Caller, InvokeArgument, InvokeRequest};
use tilo_lock::MutexRecover as _;

use super::*;

/// Answers each round with the next scripted message, keeping every request it was sent.
struct Script {
    answers: VecDeque<Message>,
    asked: Arc<Mutex<Vec<ModelRequest>>>,
}

impl Model for Script {
    fn turn(
        &mut self,
        request: &ModelRequest,
        _: &str,
        _: Instant,
    ) -> Result<ModelResponse, String> {
        self.asked.lock_recover().push(request.clone());
        let message = self.answers.pop_front().ok_or("script exhausted")?;
        let calls = message
            .content
            .iter()
            .any(|content| matches!(content, Content::ToolCall(_)));
        Ok(ModelResponse {
            message,
            stop_reason: if calls {
                StopReason::ToolCalls
            } else {
                StopReason::EndTurn
            },
            usage: Usage::default(),
            cost_micros: 10,
        })
    }
}

fn calling(id: &str, name: &str, input: Value) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![Content::ToolCall(ToolCall {
            id: id.into(),
            name: name.into(),
            input,
            extra_content: None,
        })],
    }
}

#[derive(Default)]
struct Told {
    stop: AtomicBool,
    calls: Mutex<Vec<String>>,
    spent: Mutex<u64>,
    /// Whether its person wrote while the model answered: what they wrote comes at the next step.
    waiting: AtomicBool,
    /// Whether the turn took its first step, before which nothing came.
    stepped: AtomicBool,
}

/// Where the tests' turns are answered: the session over a carried link.
type Answering = SessionTools<tilo_session_actions::SessionActions>;

impl Doing<Answering> for Told {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }
    async fn calling(&self, call: &ToolCall, _: Step) {
        self.calls.lock_recover().push(call.name.clone());
    }
    fn spent(&self, total: u64) {
        *self.spent.lock_recover() = total;
    }
    fn waiting(&self) -> bool {
        self.waiting.load(Ordering::SeqCst)
    }
    async fn went_on(&self) -> Result<Option<WentOn<Answering>>, String> {
        if !self.stepped.swap(true, Ordering::SeqCst) {
            return Ok(None);
        }
        Ok(self.waiting.swap(false, Ordering::SeqCst).then(|| WentOn {
            told: "And the weather?".to_owned(),
            tools: None,
        }))
    }
    async fn own(&self, call: &ToolCall) -> Option<Answer> {
        (call.name == "search_web").then(|| Answer {
            text: "three pages".into(),
            ..Answer::default()
        })
    }
}

/// The tools the session lists.
const LISTED: [&str; 5] = ["tilo_ls", "tilo_read", "tilo_do", "tilo_find", "tilo_view"];

/// What the stand-in session answers each of the agent's steps with: its tools, and each call's
/// answer naming the tool.
fn answer_of_session(request: &InvokeRequest) -> ActionOutput {
    let value = if request.target.address.action == wire::LIST {
        serde_json::to_value(wire::ToolList {
            tools: LISTED
                .iter()
                .map(|name| wire::Tool {
                    name: (*name).to_owned(),
                    description: String::new(),
                    input_schema: json!({"type": "object"}),
                })
                .collect(),
        })
    } else {
        let InvokeArgument::Json(argument) = &request.argument else {
            panic!("a call carries its tool: {request:?}");
        };
        let asked: wire::ToolCall =
            serde_json::from_value(Value::Object(argument.clone())).unwrap();
        serde_json::to_value(wire::ToolAnswer {
            text: format!("{} answered", asked.name),
            ..wire::ToolAnswer::default()
        })
    };
    ActionOutput {
        value: Some(value.unwrap()),
        resources: Vec::new(),
    }
}

/// The stand-in session behind the agent's link, answering its steps.
fn session() -> Arc<TestProvider> {
    Arc::new(TestProvider::new().completing(answer_of_session))
}

/// The tools the session answers, reached through a carried link that a thread of the test
/// carries to `provider`, as the session would.
fn served(provider: Arc<TestProvider>) -> Answering {
    let workspace = WorkspaceIdOf::new();
    let (actions, carried) = carried(workspace, || {});
    let caller = Caller::local_person("person").in_workspace(workspace.to_string());
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        loop {
            for message in carried.take_outbox() {
                if let Some(answer) = tilo_actions::remote::serve(&provider, &caller, message) {
                    let carried = Arc::clone(&carried);
                    runtime.spawn(async move {
                        if let Some(answer) = answer.await {
                            carried.hear(answer);
                        }
                    });
                }
            }
            if Arc::strong_count(&carried) == 1 {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    let caller = Caller::local_person("agent-1").in_workspace(workspace.to_string());
    SessionTools::new(actions, caller)
}

type WorkspaceIdOf = collab_types::WorkspaceId;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_lists_the_sessions_tools_and_calls_each_there() {
    let provider = session();
    let server = served(Arc::clone(&provider));
    let asked = Arc::new(Mutex::new(Vec::new()));
    let model = Arc::new(Mutex::new(Script {
        answers: VecDeque::from([
            calling("1", "tilo_ls", json!({})),
            calling("2", "search_web", json!({"query": "jam"})),
            Message::text(Role::Assistant, "Listed the apps."),
        ]),
        asked: Arc::clone(&asked),
    }));
    let told = Told::default();
    let mut turn = Asked::new("You work in a workspace.", "What apps are there?");
    turn.history = vec![
        Message::text(Role::User, "Can you see the workspace?"),
        Message::text(Role::Assistant, "Yes."),
    ];
    turn.own_tools.push(ToolDefinition {
        name: "search_web".into(),
        description: "Search the web".into(),
        input_schema: json!({"type": "object"}),
    });
    let (outcome, cost) = work(&server, model, &turn, &told).await;
    assert_eq!(outcome, Outcome::Answered("Listed the apps.".into()));
    assert_eq!(cost, 30);
    assert_eq!(*told.spent.lock_recover(), 30);
    assert_eq!(*told.calls.lock_recover(), ["tilo_ls", "search_web"]);
    let asked = asked.lock_recover().clone();
    assert_eq!(
        asked[0].messages,
        [
            Message::text(Role::System, "You work in a workspace."),
            Message::text(Role::User, "Can you see the workspace?"),
            Message::text(Role::Assistant, "Yes."),
            Message::text(Role::User, "What apps are there?"),
        ],
    );
    let tools: Vec<&str> = asked[0]
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect();
    for wanted in LISTED.iter().chain(&["search_web"]) {
        assert!(tools.contains(wanted), "{tools:?}");
    }
    // The model read what each call answered: the session's tool, then the caller's own tool.
    let result = |request: &ModelRequest| match request.messages.last().map(|m| &m.content[0]) {
        Some(Content::ToolResult(result)) => result.clone(),
        other => panic!("no tool result: {other:?}"),
    };
    assert_eq!(result(&asked[1]).content, "tilo_ls answered");
    assert_eq!(result(&asked[2]).content, "three pages");
    // The session was asked for its tools, then to take the one workspace call; the caller's own tool
    // never reached it.
    let steps: Vec<(String, Value)> = provider
        .asked()
        .into_iter()
        .map(|request| {
            let argument = match request.argument {
                InvokeArgument::Json(map) => Value::Object(map),
                _ => Value::Null,
            };
            (request.target.address.action, argument)
        })
        .collect();
    assert_eq!(
        steps,
        [
            (wire::LIST.to_owned(), json!({})),
            (
                wire::CALL.to_owned(),
                json!({"name": "tilo_ls", "arguments": {}})
            ),
        ]
    );
    assert!(
        provider
            .asked()
            .iter()
            .all(|request| request.target.provider == wire::PROVIDER),
        "every step is the agent tools' own"
    );
}

/// A call the session refuses or fails reaches the model as a failed answer saying why, and the turn
/// goes on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_the_session_refuses_is_a_failed_answer() {
    let provider = Arc::new(TestProvider::new().answering(|_, request| {
        tilo_actions::remote::Pending::ready(Err(tilo_actions::remote::ActionCallError::Refused {
            reason: format!(
                "the workspace does not let this agent take {}",
                request.target.address.action
            ),
        }))
    }));
    let answering = served(provider);
    let answer = answering
        .call("tilo_do", json!({"path": "notes#open"}))
        .await;
    assert!(answer.is_error, "{answer:?}");
    assert!(
        answer
            .text
            .contains("does not let this agent take call-tool"),
        "{}",
        answer.text
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stopped_turn_begins_no_tool() {
    let server = served(session());
    let model = Arc::new(Mutex::new(Script {
        answers: VecDeque::from([calling("1", "tilo_do", json!({"path": "notes#open"}))]),
        asked: Arc::default(),
    }));
    let told = Told::default();
    told.stop.store(true, Ordering::SeqCst);
    let (outcome, _) = work(&server, model, &Asked::new("s", "r"), &told).await;
    assert_eq!(outcome, Outcome::Cancelled);
    assert!(told.calls.lock_recover().is_empty());
}

/// A turn that has spent what it may stops before its next round, says so, and asks the model
/// nothing more: an agent working through the night cannot spend past its daily limit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_stops_at_its_budget_and_asks_nothing_more() {
    let server = served(session());
    let asked = Arc::new(Mutex::new(Vec::new()));
    let model = Arc::new(Mutex::new(Script {
        // Its one round costs ten (each scripted answer does) and calls a tool, so a second
        // round would be due: the budget of five stops it before that round is asked for.
        answers: VecDeque::from([calling("1", "tilo_ls", json!({}))]),
        asked: Arc::clone(&asked),
    }));
    let told = Told::default();
    let turn = Asked {
        budget: Some(5),
        ..Asked::new("s", "r")
    };
    let (outcome, cost) = work(&server, model, &turn, &told).await;
    assert!(
        matches!(&outcome, Outcome::Stopped(why) if why.contains("spending limit")),
        "{outcome:?}"
    );
    assert_eq!((cost, *told.spent.lock_recover()), (10, 10));
    assert_eq!(
        asked.lock_recover().len(),
        1,
        "the model is asked for the round that spent, and no other"
    );
}

/// A model that only ever calls tools ends the turn at its round bound rather than looping for
/// ever: it is then asked once more, with no tools, to say where it got to, and its person reads
/// that.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_of_endless_tool_calls_stops_at_its_round_bound_and_says_where_it_got() {
    let server = served(session());
    let asked = Arc::new(Mutex::new(Vec::new()));
    let model = Arc::new(Mutex::new(Script {
        // One round's answer for each round the bound lets it take, then where it got to.
        answers: VecDeque::from([
            calling("1", "tilo_ls", json!({})),
            calling("2", "tilo_ls", json!({})),
            Message::text(
                Role::Assistant,
                "I listed the apps; the notes are still open.",
            ),
        ]),
        asked: Arc::clone(&asked),
    }));
    let told = Told::default();
    let turn = Asked {
        most_rounds: Some(2),
        ..Asked::new("s", "r")
    };
    let (outcome, _) = work(&server, model, &turn, &told).await;
    assert_eq!(
        outcome,
        Outcome::Stopped("I listed the apps; the notes are still open.".into())
    );
    let asked = asked.lock_recover().clone();
    assert_eq!(asked.len(), 3, "no round of tools past the bound is asked");
    assert_eq!(
        asked.last().map(|last| last.tool_choice),
        Some(ToolChoice::None),
        "the last ask calls no tool"
    );
}

/// A turn whose model says nothing once out of rounds tells its person how to let it go on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_out_of_rounds_that_says_nothing_tells_how_to_go_on() {
    let server = served(session());
    let model = Arc::new(Mutex::new(Script {
        answers: VecDeque::from([calling("1", "tilo_ls", json!({}))]),
        asked: Arc::default(),
    }));
    let turn = Asked {
        most_rounds: Some(1),
        ..Asked::new("s", "r")
    };
    let (outcome, _) = work(&server, model, &turn, &Told::default()).await;
    assert!(
        matches!(&outcome, Outcome::Stopped(why) if why.contains("many steps")),
        "{outcome:?}"
    );
}

/// What its person writes while the agent works reaches the model at its next step, and an
/// answer given while more waits is not the turn's answer: the turn goes on with what came.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn what_its_person_wrote_meanwhile_reaches_the_next_step() {
    let server = served(session());
    let asked = Arc::new(Mutex::new(Vec::new()));
    let model = Arc::new(Mutex::new(Script {
        answers: VecDeque::from([
            Message::text(Role::Assistant, "Here are the apps."),
            Message::text(Role::Assistant, "Here is the weather too."),
        ]),
        asked: Arc::clone(&asked),
    }));
    let told = Told::default();
    told.waiting.store(true, Ordering::SeqCst);
    let (outcome, _) = work(&server, model, &Asked::new("s", "r"), &told).await;
    assert_eq!(
        outcome,
        Outcome::Answered("Here is the weather too.".into())
    );
    let asked = asked.lock_recover().clone();
    assert_eq!(asked.len(), 2);
    let second = format!("{:?}", asked[1].messages);
    assert!(
        second.contains(CAME_SINCE) && second.contains("And the weather?"),
        "{second}"
    );
}

/// An answer with no words is asked once to say what it did, and the turn answers with the
/// words that follow: a person is left with what their agent did, not a blank answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_answer_with_no_words_is_asked_once_to_say_what_it_did() {
    let server = served(session());
    let asked = Arc::new(Mutex::new(Vec::new()));
    let model = Arc::new(Mutex::new(Script {
        answers: VecDeque::from([
            Message {
                role: Role::Assistant,
                content: Vec::new(),
            },
            Message::text(Role::Assistant, "I filed the issue."),
        ]),
        asked: Arc::clone(&asked),
    }));
    let told = Told::default();
    let (outcome, _) = work(&server, model, &Asked::new("s", "r"), &told).await;
    assert_eq!(outcome, Outcome::Answered("I filed the issue.".into()));
    let asked = asked.lock_recover().clone();
    assert_eq!(asked.len(), 2);
    assert_eq!(
        asked[1].messages.last(),
        Some(&Message::text(Role::User, super::SAY_WHAT_YOU_DID)),
        "the model is told the person reads words"
    );
}

/// A tool's answer longer than a model reads is cut on a character boundary, keeps as much as
/// fits, and says it was cut: a model never reads half a character, nor the whole of a listing it
/// could not take in.
#[test]
fn a_long_tool_answer_is_cut_at_a_character_and_says_so() {
    let notice = "\n…(cut: ask for less)";
    // Three-byte characters behind one ASCII byte, more of them than a model reads: the cut
    // falls where a character would be split if it counted bytes alone.
    let text = format!("x{}", "€".repeat(20_000));
    let cut_text = cut(text.clone(), MOST_RESULT_BYTES);
    let kept = cut_text
        .strip_suffix(notice)
        .unwrap_or_else(|| panic!("a long answer says it was cut: {cut_text}"));
    assert!(kept.starts_with('x'), "the answer is kept from its start");
    assert!(
        text.starts_with(kept),
        "every character kept is whole: {} bytes kept",
        kept.len()
    );
    assert!(kept.len() <= MOST_RESULT_BYTES, "{} bytes kept", kept.len());
    assert!(
        kept.len().saturating_add(3) > MOST_RESULT_BYTES,
        "as much as fits is kept: {} bytes kept",
        kept.len()
    );
    assert_eq!(
        cut("It is there.".into(), MOST_RESULT_BYTES),
        "It is there.",
        "a short answer is whole"
    );
}

#[test]
fn old_answers_give_way_and_the_latest_stays() {
    let answer = |id: &str| Message {
        role: Role::Tool,
        content: vec![Content::ToolResult(ToolResult {
            call_id: id.into(),
            content: "x".repeat(1000),
            is_error: false,
        })],
    };
    let mut messages = vec![
        Message::text(Role::User, "request"),
        answer("1"),
        answer("2"),
        answer("3"),
    ];
    compact::compact(&mut messages, 2_100);
    let contents: Vec<&str> = messages
        .iter()
        .skip(1)
        .map(|message| match &message.content[0] {
            Content::ToolResult(result) => result.content.as_str(),
            _ => "",
        })
        .collect();
    assert_eq!(contents[0], compact::LEFT_OUT);
    assert_eq!(contents[2].len(), 1000, "the latest answer stays whole");
}

mod hooks;
