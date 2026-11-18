//! A turn over a provider reached through a carried link: the model's calls go through the MCP
//! server's tools to the provider, its answer ends the turn, a stop ends it, and a long turn keeps
//! within its bound.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};

use agent_provider_config::protocol::Usage;
use tilo_actions::remote::{Caller, TestProvider};

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
}

impl Doing for Told {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }
    fn calling(&self, call: &ToolCall) {
        self.calls.lock_recover().push(call.name.clone());
    }
    fn spent(&self, total: u64) {
        *self.spent.lock_recover() = total;
    }
    fn own(&self, call: &ToolCall) -> Option<Answer> {
        (call.name == "search_web").then(|| Answer {
            text: "three pages".into(),
            ..Answer::default()
        })
    }
}

/// A server whose provider is reached through a carried link that a thread of the test carries to
/// `provider`, as a host would.
fn served(provider: Arc<TestProvider>) -> McpServer<tilo_session_client::remote::SessionActions> {
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
    McpServer::new(actions, workspace.to_string(), "person".into())
}

type WorkspaceIdOf = collab_types::WorkspaceId;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_lists_the_servers_tools_and_runs_each_call_through_them() {
    let provider = Arc::new(TestProvider::new());
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
    let tools: Vec<&str> = asked[0]
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect();
    for wanted in [
        "tilo_ls",
        "tilo_read",
        "tilo_do",
        "tilo_find",
        "tilo_view",
        "search_web",
    ] {
        assert!(tools.contains(&wanted), "{tools:?}");
    }
    // The model read what each call answered: the server's listing, then the caller's own tool.
    let result = |request: &ModelRequest| match request.messages.last().map(|m| &m.content[0]) {
        Some(Content::ToolResult(result)) => result.clone(),
        other => panic!("no tool result: {other:?}"),
    };
    assert!(!result(&asked[1]).content.is_empty());
    assert_eq!(result(&asked[2]).content, "three pages");
    assert!(
        !provider.callers().is_empty(),
        "the listing asked the provider over the carried link"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stopped_turn_begins_no_tool() {
    let provider = Arc::new(TestProvider::new());
    let server = served(provider);
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
