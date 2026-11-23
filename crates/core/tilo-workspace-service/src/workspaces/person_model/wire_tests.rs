//! A workspace's model asked for real: the host's own provider client against
//! servers that speak each provider's wire. What a workspace keeps in its
//! settings and vault reaches the provider in the provider's own dialect,
//! and the stream, the tool calls, the tokens and the cost come back.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "the host handle a test binds is kept until the test ends; dropping it earlier closes the host it drives"
)]

use std::fmt::Write as _;

use agent_provider_config::protocol::{
    Content, Message, Role, StreamState, ToolChoice, ToolDefinition, ToolResult,
};
use serde_json::{Value, json};
use test_http::{Reply, Responder};

use super::scripted::Linking;
use super::*;
use crate::workspaces::fixtures::TestHost;
use crate::workspaces::tests::{governor, test_host};

const ADA: WorkspaceId = WorkspaceId::from_u128(7);
const KEY: &str = "sk-ada-wire-secret";
const OPENAI: ProviderProtocol = ProviderProtocol::OpenAiCompatible;

fn lookup() -> ToolDefinition {
    ToolDefinition {
        name: "lookup".into(),
        description: "Look a thing up".into(),
        input_schema: json!({
            "type": "object",
            "properties": {"count": {"type": "integer"}},
            "required": ["count"],
        }),
    }
}

fn asking(text: &str, tools: Vec<ToolDefinition>) -> ModelRequest {
    ModelRequest {
        messages: vec![
            Message::text(Role::System, "You work for Ada."),
            Message::text(Role::User, text),
        ],
        tools,
        tool_choice: ToolChoice::Auto,
        max_output_tokens: 256,
    }
}

fn soon() -> Instant {
    tilo_clock::now()
        .checked_add(tilo_test_env::hang_bound())
        .unwrap()
}

/// A host whose operator lets it call the test's provider on this machine,
/// with that provider linked as the workspace's own: its address, its key in
/// its vault and `model` as what its work runs on.
async fn linked(
    (name, protocol): (&str, ProviderProtocol),
    endpoint: &str,
    model: &str,
) -> (tempfile::TempDir, TestHost) {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    server
        .workspaces()
        .create_workspace("Ada", Some(&ADA.to_string()))
        .await
        .unwrap();
    server.workspaces().call_private_model_addresses(true);
    server
        .workspaces()
        .link_model_for(
            &ADA.to_string(),
            &Linking {
                name,
                protocol,
                endpoint,
                key: Some(KEY),
                models: &[model],
            },
            true,
        )
        .await
        .unwrap();
    (dir, server)
}

/// A stream as an OpenAI-compatible provider writes one.
fn openai_stream(chunks: &[Value]) -> Reply {
    let mut body = String::new();
    for chunk in chunks {
        write!(body, "data: {chunk}\n\n").unwrap();
    }
    body.push_str("data: [DONE]\n\n");
    Reply::status(200)
        .header("Content-Type", "text/event-stream")
        .body(body)
}

fn chunk(model: &str, delta: &Value, finish: &Value) -> Value {
    json!({"id": "reply", "object": "chat.completion.chunk", "created": 1, "model": model,
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
}

fn usage(model: &str, prompt: u64, completion: u64) -> Value {
    json!({"id": "reply", "object": "chat.completion.chunk", "created": 1, "model": model,
        "choices": [],
        "usage": {"prompt_tokens": prompt, "completion_tokens": completion,
            "total_tokens": prompt.saturating_add(completion)}})
}

/// A tool call as an OpenAI-compatible provider streams one, with what the
/// provider adds to it.
fn calling(model: &str, extra: Option<&Value>) -> Value {
    let mut call = json!({"index": 0, "id": "call_1", "type": "function",
        "function": {"name": "lookup", "arguments": "{\"count\":2}"}});
    if let Some(extra) = extra {
        call.as_object_mut()
            .unwrap()
            .insert("extra_content".into(), extra.clone());
    }
    chunk(
        model,
        &json!({"role": "assistant", "tool_calls": [call]}),
        &Value::Null,
    )
}

/// A stream as Anthropic's Messages API writes one.
fn anthropic_stream(events: &[Value]) -> Reply {
    let mut body = String::new();
    for event in events {
        let kind = event["type"].as_str().unwrap();
        write!(body, "event: {kind}\ndata: {event}\n\n").unwrap();
    }
    Reply::status(200)
        .header("Content-Type", "text/event-stream")
        .body(body)
}

fn body_of(request: &test_http::Request) -> Value {
    serde_json::from_slice(&request.body).unwrap()
}

/// The key travels in `header` and nowhere else of `request`.
fn keyed_only_in(request: &test_http::Request, header: &str, value: &str) {
    assert_eq!(request.header(header), Some(value));
    for (name, sent) in &request.headers {
        if !name.eq_ignore_ascii_case(header) {
            assert!(!sent.contains(KEY), "{name} carries the key");
        }
    }
    assert!(!request.target.contains(KEY), "the address carries the key");
    assert!(
        !request.body_text().contains(KEY),
        "the body carries the key"
    );
}

/// A script that answers for the workspace's provider, and that provider's
/// id in its settings.
fn scripted_for_ada(
    workspaces: &Workspaces,
    said: impl IntoIterator<Item = &'static str>,
) -> (std::sync::Arc<super::scripted::ScriptedProvider>, String) {
    use super::scripted::{Reply as Said, ScriptedProvider};
    let scripted = ScriptedProvider::answering(said.into_iter().map(Said::text));
    workspaces.answer_models_with(scripted.clone());
    let root = workspaces.workspace_root(&ADA.to_string());
    let provider = crate::workspaces::person_settings::provider_config(&root)
        .profiles
        .first()
        .map(|profile| profile.id.clone())
        .unwrap();
    (scripted, provider)
}

/// The workspace's provider with its key in its vault, as its settings keep
/// it.
const HERS: (&str, &str) = (
    "https://generativelanguage.googleapis.com/v1beta/openai",
    "gemini-main",
);

/// One piece of work asks its model many times one after the other: the key is
/// taken from the workspace's vault, its use written, for the first ask and not
/// again while its entry holds what it held. Other work takes it for itself.
#[tokio::test]
async fn the_asks_of_one_piece_of_work_take_the_key_once() {
    let (_dir, server) = linked(("Google", OPENAI), HERS.0, HERS.1).await;
    let workspaces = server.workspaces();
    let (scripted, _) = scripted_for_ada(workspaces, ["one", "two", "three", "four"]);

    let model = workspaces.workspace_model(ADA, Interaction::Task);
    model.linked().unwrap();
    for _ in 0..3 {
        model
            .answer(&asking("Hello", Vec::new()), soon(), &mut |_| {})
            .await
            .unwrap();
    }
    let asked = scripted.asked();
    assert_eq!(asked.len(), 3);
    assert!(
        asked.iter().all(|asked| asked.key.as_deref() == Some(KEY)),
        "{asked:?}"
    );
    assert_eq!(
        model.kept.taken(),
        1,
        "the key is taken once for the work's three asks"
    );

    let other = workspaces.workspace_model(ADA, Interaction::Task);
    other.linked().unwrap();
    other
        .answer(&asking("Hello", Vec::new()), soon(), &mut |_| {})
        .await
        .unwrap();
    assert_eq!(other.kept.taken(), 1, "other work takes the key for itself");
}

/// A key its members replace or remove while work goes on is new, or gone,
/// at the very next ask: the provider is asked with the new key, and is not
/// asked at all once the key is removed.
#[tokio::test]
async fn a_key_replaced_or_removed_between_two_asks_is_not_used_by_the_next() {
    use crate::workspaces::person_settings::{ProviderKeys, remove_key, set_key};
    use tilo_actions::remote::person_models::SetKey;

    let (_dir, server) = linked(("Google", OPENAI), HERS.0, HERS.1).await;
    let workspaces = server.workspaces();
    let (scripted, provider) = scripted_for_ada(workspaces, ["one", "two", "never"]);
    let root = workspaces.workspace_root(&ADA.to_string());

    let model = workspaces.workspace_model(ADA, Interaction::Task);
    model
        .answer(&asking("Hello", Vec::new()), soon(), &mut |_| {})
        .await
        .unwrap();

    let workspace = ADA.to_string();
    let keys = ProviderKeys::of(workspaces.vault_at(&workspace), "");
    set_key(
        (keys, &root),
        SetKey {
            provider: provider.clone(),
            key: "sk-ada-replaced".into(),
        },
        None,
    )
    .await
    .unwrap();
    model
        .answer(&asking("Hello", Vec::new()), soon(), &mut |_| {})
        .await
        .unwrap();
    let asked_with: Vec<Option<String>> = scripted
        .asked()
        .into_iter()
        .map(|asked| asked.key)
        .collect();
    assert_eq!(
        asked_with,
        [Some(KEY.to_owned()), Some("sk-ada-replaced".to_owned())]
    );

    remove_key((keys, &root), &provider).await.unwrap();
    let refused = model
        .answer(&asking("Hello", Vec::new()), soon(), &mut |_| {})
        .await
        .unwrap_err();
    assert_eq!(
        refused,
        ModelRefusal::NoKey {
            provider: "Google".into()
        }
    );
    assert_eq!(
        scripted.asked().len(),
        2,
        "a provider whose key was removed is not asked"
    );
}

/// An OpenAI-compatible provider is asked with a bearer key at its chat
/// completions, and what it streams arrives as it comes: the words, then the
/// whole answer with its tokens and what they cost at the model's price.
#[tokio::test]
async fn an_openai_compatible_provider_streams_its_answer_and_its_cost() {
    let model = "gpt-4o-mini";
    let responder = Responder::start(vec![openai_stream(&[
        chunk(
            model,
            &json!({"role": "assistant", "content": "Good "}),
            &Value::Null,
        ),
        chunk(model, &json!({"content": "morning"}), &Value::Null),
        chunk(model, &json!({}), &json!("stop")),
        usage(model, 1_000, 500),
    ])]);
    let (_dir, server) = linked(
        ("A provider", OPENAI),
        &format!("{}/v1", responder.url()),
        model,
    )
    .await;
    let asked = server
        .workspaces()
        .workspace_model(ADA, Interaction::Request);

    let mut stream = StreamState::default();
    let mut pieces = Vec::new();
    let answer = asked
        .answer(&asking("Good morning", Vec::new()), soon(), &mut |event| {
            stream.apply(&event);
            pieces.push(event);
        })
        .await
        .unwrap();
    assert_eq!(
        pieces,
        vec![
            StreamEvent::TextDelta {
                text: "Good ".into()
            },
            StreamEvent::TextDelta {
                text: "morning".into()
            },
        ]
    );
    assert_eq!(stream.text, "Good morning");
    assert_eq!(answer.text(), "Good morning");
    assert_eq!(answer.response.usage.input_tokens, 1_000);
    assert_eq!(answer.response.usage.output_tokens, 500);
    // A thousand tokens in at 0.15 and five hundred out at 0.60 a million.
    assert_eq!(answer.response.cost_micros, 450);

    let request = responder.next_request();
    assert_eq!(request.method, "POST");
    assert_eq!(request.target, "/v1/chat/completions");
    keyed_only_in(&request, "authorization", &format!("Bearer {KEY}"));
    let sent = body_of(&request);
    assert_eq!(sent["model"], model);
    assert_eq!(sent["stream"], true);
    assert!(
        sent["messages"].to_string().contains("Good morning"),
        "{sent}"
    );
}

/// Every file under `dir` whose bytes hold `needle`.
fn files_holding(dir: &std::path::Path, needle: &str) -> Vec<std::path::PathBuf> {
    let mut holding = Vec::new();
    let mut waiting = vec![dir.to_path_buf()];
    while let Some(next) = waiting.pop() {
        for entry in std::fs::read_dir(&next).into_iter().flatten().flatten() {
            let path = entry.path();
            match std::fs::metadata(&path) {
                Ok(metadata) if metadata.is_dir() => waiting.push(path),
                Ok(metadata) if metadata.is_file() => {
                    let bytes = std::fs::read(&path).unwrap();
                    if bytes
                        .windows(needle.len())
                        .any(|window| window == needle.as_bytes())
                    {
                        holding.push(path);
                    }
                }
                _ => {}
            }
        }
    }
    holding
}

/// An agent's turn asks its workspace's model with the key the provider names in the
/// workspace's vault, read from the vault at the ask: the provider's settings name the entry
/// and hold no key, the host keeps no vault of its own beside the workspace's, and no file on
/// the host's disk holds the key.
#[tokio::test]
async fn a_turn_asks_with_the_key_only_the_workspaces_vault_holds() {
    let model = "gpt-4o-mini";
    let responder = Responder::start(vec![openai_stream(&[
        chunk(
            model,
            &json!({"role": "assistant", "content": "Sorted."}),
            &Value::Null,
        ),
        chunk(model, &json!({}), &json!("stop")),
    ])]);
    let (dir, server) = linked(
        ("A provider", OPENAI),
        &format!("{}/v1", responder.url()),
        model,
    )
    .await;
    let workspaces = server.workspaces();

    let answer = workspaces
        .workspace_model(ADA, Interaction::Task)
        .answer(&asking("Sort my inbox", Vec::new()), soon(), &mut |_| {})
        .await
        .unwrap();
    assert_eq!(answer.text(), "Sorted.");
    keyed_only_in(
        &responder.next_request(),
        "authorization",
        &format!("Bearer {KEY}"),
    );

    let workspace = ADA.to_string();
    let root = workspaces.workspace_root(&workspace);
    let profiles = super::super::person_settings::provider_config(&root).profiles;
    let named = super::super::person_settings::key_reference(&profiles[0])
        .expect("the provider names an entry of the workspace's vault");
    let vault = workspaces.open_vault(&workspace).await.unwrap();
    assert_eq!(
        vault.entry(named.id()).map(|entry| entry.password.as_str()),
        Some(KEY)
    );
    assert!(
        !accounts::vault_dir_in(&root).exists(),
        "the host keeps a vault of its own"
    );
    assert_eq!(
        files_holding(dir.path(), KEY),
        Vec::<std::path::PathBuf>::new()
    );
}

/// A tool call an OpenAI-compatible provider streams comes back whole, with
/// its arguments as the provider wrote them; what streamed of it is for
/// showing only.
#[tokio::test]
async fn an_openai_compatible_providers_tool_call_comes_back_whole() {
    let model = "gpt-4o-mini";
    let responder = Responder::start(vec![openai_stream(&[
        calling(model, None),
        chunk(model, &json!({}), &json!("tool_calls")),
        usage(model, 2_000, 100),
    ])]);
    let (_dir, server) = linked(
        ("A provider", OPENAI),
        &format!("{}/v1", responder.url()),
        model,
    )
    .await;

    let mut deltas = Vec::new();
    let calling = server
        .workspaces()
        .workspace_model(ADA, Interaction::Request)
        .answer(
            &asking("Look it up", vec![lookup()]),
            soon(),
            &mut |event| deltas.push(event),
        )
        .await
        .unwrap();
    let calls = calling.response.tool_calls().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        (calls[0].id.as_str(), calls[0].name.as_str()),
        ("call_1", "lookup")
    );
    assert_eq!(calls[0].input, json!({"count": 2}));
    assert!(!deltas.is_empty());
    assert!(
        deltas
            .iter()
            .all(|event| matches!(event, StreamEvent::ToolCallDelta { .. })),
        "{deltas:?}"
    );
    let sent = body_of(&responder.next_request());
    assert_eq!(sent["tools"][0]["function"]["name"], "lookup");
}

/// Anthropic is asked at its Messages API with the key in its own header,
/// and its stream of typed events arrives as words, a tool call and usage.
#[tokio::test]
async fn anthropic_is_asked_at_its_messages_api() {
    let model = "claude-sonnet-4-5";
    let responder = Responder::start(vec![anthropic_stream(&[
        json!({"type": "message_start", "message": {"id": "msg_1", "type": "message",
            "role": "assistant", "model": model, "content": [], "stop_reason": null,
            "stop_sequence": null, "usage": {"input_tokens": 1_000, "output_tokens": 1}}}),
        json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": "Looking it up."}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "content_block_start", "index": 1,
            "content_block": {"type": "tool_use", "id": "toolu_1", "name": "lookup", "input": {}}}),
        json!({"type": "content_block_delta", "index": 1,
            "delta": {"type": "input_json_delta", "partial_json": "{\"count\":"}}),
        json!({"type": "content_block_delta", "index": 1,
            "delta": {"type": "input_json_delta", "partial_json": " 2}"}}),
        json!({"type": "content_block_stop", "index": 1}),
        json!({"type": "message_delta",
            "delta": {"stop_reason": "tool_use", "stop_sequence": null},
            "usage": {"output_tokens": 200}}),
        json!({"type": "message_stop"}),
    ])]);
    let (_dir, server) = linked(
        ("Anthropic", ProviderProtocol::Anthropic),
        &responder.url(),
        model,
    )
    .await;

    let mut stream = StreamState::default();
    let answer = server
        .workspaces()
        .workspace_model(ADA, Interaction::Subtask)
        .answer(
            &asking("Look it up", vec![lookup()]),
            soon(),
            &mut |event| stream.apply(&event),
        )
        .await
        .unwrap();
    assert_eq!(stream.text, "Looking it up.");
    let calls = answer.response.tool_calls().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        (calls[0].id.as_str(), calls[0].name.as_str()),
        ("toolu_1", "lookup")
    );
    assert_eq!(calls[0].input, json!({"count": 2}));
    assert_eq!(answer.response.usage.input_tokens, 1_000);
    assert_eq!(answer.response.usage.output_tokens, 200);
    // A thousand tokens in at 3 and two hundred out at 15 a million.
    assert_eq!(answer.response.cost_micros, 6_000);

    let request = responder.next_request();
    assert_eq!(request.target, "/v1/messages");
    keyed_only_in(&request, "x-api-key", KEY);
    assert!(request.header("authorization").is_none());
    assert!(request.header("anthropic-version").is_some());
    let sent = body_of(&request);
    assert_eq!(sent["model"], model);
    assert_eq!(sent["stream"], true);
    assert_eq!(sent["max_tokens"], 256);
    assert_eq!(sent["tools"][0]["name"], "lookup");
    assert!(sent.to_string().contains("You work for Ada."), "{sent}");
}

/// A turn in which Anthropic's `model` calls the lookup tool, as its stream
/// writes it.
fn anthropic_calling(model: &str) -> Vec<Value> {
    vec![
        json!({"type": "message_start", "message": {"id": "msg_1", "type": "message",
            "role": "assistant", "model": model, "content": [], "stop_reason": null,
            "stop_sequence": null, "usage": {"input_tokens": 10, "output_tokens": 1}}}),
        json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "tool_use", "id": "toolu_1", "name": "lookup", "input": {}}}),
        json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "input_json_delta", "partial_json": "{\"count\": 2}"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta",
            "delta": {"stop_reason": "tool_use", "stop_sequence": null},
            "usage": {"output_tokens": 20}}),
        json!({"type": "message_stop"}),
    ]
}

/// Anthropic's newest models refuse a request that says they must call a
/// tool, which an agent's loop says on every turn that offers tools. Asked
/// of one of them the choice is left to the model, so it can work at all;
/// an earlier model is still told it must call one.
#[tokio::test]
async fn a_claude_model_that_refuses_a_forced_tool_call_is_left_to_choose() {
    for (model, choice) in [("claude-opus-5-5", "auto"), ("claude-sonnet-4-6", "any")] {
        let responder = Responder::start(vec![anthropic_stream(&anthropic_calling(model))]);
        let (_dir, server) = linked(
            ("Anthropic", ProviderProtocol::Anthropic),
            &responder.url(),
            model,
        )
        .await;
        let mut request = asking("Look it up", vec![lookup()]);
        request.tool_choice = ToolChoice::Required;
        let answer = server
            .workspaces()
            .workspace_model(ADA, Interaction::Task)
            .answer(&request, soon(), &mut |_| {})
            .await
            .unwrap();
        assert_eq!(answer.response.tool_calls().unwrap().len(), 1, "{model}");
        let sent = body_of(&responder.next_request());
        assert_eq!(sent["tool_choice"]["type"], choice, "{model}: {sent}");
    }
}

/// Google's Gemini is asked through its OpenAI-compatible endpoint, and the
/// signature it puts on a tool call goes back with that call on the next
/// turn, without which it refuses the turn.
#[tokio::test]
async fn gemini_is_asked_through_its_openai_endpoint_and_gets_its_signature_back() {
    let model = "gemini-2.5-flash";
    let signature = json!({"google": {"thought_signature": "opaque-signed-thought"}});
    let responder = Responder::start(vec![
        openai_stream(&[
            calling(model, Some(&signature)),
            chunk(model, &json!({}), &json!("tool_calls")),
            usage(model, 1_000, 100),
        ]),
        openai_stream(&[
            chunk(
                model,
                &json!({"role": "assistant", "content": "Two."}),
                &Value::Null,
            ),
            chunk(model, &json!({}), &json!("stop")),
            usage(model, 1_200, 10),
        ]),
    ]);
    let endpoint = format!("{}/v1beta/openai", responder.url());
    let (_dir, server) = linked(("Google", OPENAI), &endpoint, model).await;
    let asked = server
        .workspaces()
        .workspace_model(ADA, Interaction::Request);

    let mut request = asking("How many?", vec![lookup()]);
    let first = asked.answer(&request, soon(), &mut |_| {}).await.unwrap();
    let call = first.response.tool_calls().unwrap()[0].clone();
    assert_eq!(call.extra_content, Some(signature.clone()));
    assert!(first.response.cost_micros > 0);
    let sent = responder.next_request();
    assert_eq!(sent.target, "/v1beta/openai/chat/completions");
    keyed_only_in(&sent, "authorization", &format!("Bearer {KEY}"));

    // The next turn carries the call as the model made it, and its result.
    request.messages.push(first.response.message.clone());
    request.messages.push(Message {
        role: Role::Tool,
        content: vec![Content::ToolResult(ToolResult {
            call_id: call.id.clone(),
            content: "2".into(),
            is_error: false,
        })],
    });
    let second = asked.answer(&request, soon(), &mut |_| {}).await.unwrap();
    assert_eq!(second.text(), "Two.");
    let sent = body_of(&responder.next_request());
    let replayed = sent["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|message| message["tool_calls"].as_array())
        .expect("the call is replayed");
    assert_eq!(replayed[0]["id"], "call_1");
    assert_eq!(replayed[0]["extra_content"], signature, "{sent}");
}

/// A provider that is busy is asked again once it says it may be, and one
/// that refuses the key is the refusal, in words that carry no key.
#[tokio::test]
async fn a_busy_provider_is_asked_again_and_a_refused_key_is_said() {
    let model = "gpt-4o-mini";
    let busy = Reply::status(429)
        .header("Content-Type", "application/json")
        .header("Retry-After", "0")
        .body(r#"{"error":{"code":"rate_limit_exceeded","type":"rate_limit_error"}}"#);
    let responder = Responder::start(vec![
        busy.clone(),
        busy,
        openai_stream(&[
            chunk(
                model,
                &json!({"role": "assistant", "content": "Now."}),
                &Value::Null,
            ),
            chunk(model, &json!({}), &json!("stop")),
            usage(model, 10, 1),
        ]),
        Reply::status(401)
            .header("Content-Type", "application/json")
            .body(format!(
                r#"{{"error":{{"message":"Incorrect API key provided: {KEY}","code":"invalid_api_key"}}}}"#
            )),
    ]);
    let (_dir, server) = linked(
        ("A provider", OPENAI),
        &format!("{}/v1", responder.url()),
        model,
    )
    .await;
    let asked = server.workspaces().workspace_model(ADA, Interaction::Task);

    let answer = asked
        .answer(&asking("When?", Vec::new()), soon(), &mut |_| {})
        .await
        .unwrap();
    assert_eq!(answer.text(), "Now.");
    assert_eq!(
        responder.drain_requests().len(),
        3,
        "asked until it answered"
    );

    let refused = asked
        .answer(&asking("Again?", Vec::new()), soon(), &mut |_| {})
        .await
        .unwrap_err();
    assert_eq!(
        refused,
        ModelRefusal::KeyRefused {
            provider: "A provider".into(),
            error: ModelError::new(ErrorKind::Authentication),
        }
    );
    assert_eq!(
        refused.words(None),
        "A provider did not accept your API key. Check the key, or use another key for A provider."
    );
    assert!(!format!("{refused:?} {refused}").contains(KEY));
}

/// A call that is let go of while its answer still streams ends the request
/// at the provider: nothing nobody reads is paid for to its end.
#[tokio::test]
async fn a_call_let_go_of_ends_the_providers_request() {
    let (closed, was_closed) = std::sync::mpsc::channel();
    let closed = std::sync::Mutex::new(closed);
    let responder = Responder::serve(move |peer| {
        let first = chunk(
            "gpt-4o-mini",
            &json!({"role": "assistant", "content": "One moment"}),
            &Value::Null,
        );
        let wrote = peer.write(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {first}\n\n"
            )
            .as_bytes(),
        );
        // The rest never comes: the provider is still writing.
        let hung_up = wrote && peer.wait_for_close();
        let _ = closed.lock().unwrap().send(hung_up);
    });
    let (_dir, server) = linked(
        ("A provider", OPENAI),
        &format!("{}/v1", responder.url()),
        "gpt-4o-mini",
    )
    .await;
    let asked = server
        .workspaces()
        .workspace_model(ADA, Interaction::Request);

    let (seen, first) = tokio::sync::oneshot::channel();
    let mut seen = Some(seen);
    let request = asking("Wait for it", Vec::new());
    {
        let mut on_event = |event: StreamEvent| {
            if let Some(seen) = seen.take() {
                let _ = seen.send(event);
            }
        };
        let answering = asked.answer(&request, soon(), &mut on_event);
        tokio::select! {
            ended = answering => panic!("the provider never ends its answer: {ended:?}"),
            event = tilo_test_env::within("the first words arrive", first) => {
                assert_eq!(
                    event.unwrap(),
                    StreamEvent::TextDelta { text: "One moment".into() }
                );
            }
        }
    }
    let hung_up =
        tokio::task::spawn_blocking(move || was_closed.recv_timeout(tilo_test_env::hang_bound()))
            .await
            .unwrap()
            .expect("the provider saw the request end");
    assert!(hung_up, "the request was ended, not left open");
}

/// On a host whose operator allows no address off the public internet, a
/// provider on the host's own machine or network is never dialled.
#[tokio::test]
async fn a_provider_on_the_hosts_own_network_is_never_dialled() {
    let responder = Responder::silent();
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    server
        .workspaces()
        .create_workspace("Ada", Some(&ADA.to_string()))
        .await
        .unwrap();
    let workspaces = server.workspaces();
    for endpoint in [
        format!("{}/v1", responder.url()),
        format!("https://127.0.0.1:{}/v1", responder.port()),
        // The address a cloud machine reads its own credentials from.
        "https://169.254.169.254/v1".to_owned(),
        // A carrier-grade private range and a mapped private address.
        "https://100.64.0.1/v1".to_owned(),
        "https://[::ffff:10.0.0.1]/v1".to_owned(),
    ] {
        let hashed = blake3::hash(endpoint.as_bytes());
        let mut first = [0; 16];
        first.copy_from_slice(&hashed.as_bytes()[..16]);
        let workspace = WorkspaceId::from_u128(u128::from_le_bytes(first));
        workspaces
            .create_workspace("Inside", Some(&workspace.to_string()))
            .await
            .unwrap();
        workspaces
            .link_model_for(
                &workspace.to_string(),
                &Linking {
                    name: "Inside",
                    protocol: OPENAI,
                    endpoint: &endpoint,
                    key: Some(KEY),
                    models: &["a-model"],
                },
                true,
            )
            .await
            .unwrap();
        let refused = workspaces
            .workspace_model(workspace, Interaction::Request)
            .answer(&asking("Hello", Vec::new()), soon(), &mut |_| {})
            .await
            .unwrap_err();
        assert_eq!(
            refused,
            ModelRefusal::AddressRefused {
                provider: "Inside".into()
            },
            "{endpoint}"
        );
    }
    responder.assert_untouched();
}

/// A provider that shows nothing is let go of after the first of the two
/// times its model's calls are held to, and the model's next call waits the
/// longer one: a provider that shows nothing while it works is let go of
/// once, not every time.
#[tokio::test]
async fn a_silent_provider_is_let_go_of_and_the_next_call_waits_longer() {
    let responder = Responder::serve(|peer| {
        let _closed = peer.wait_for_close();
    });
    let (_dir, server) = linked(
        ("A provider", OPENAI),
        &format!("{}/v1", responder.url()),
        "gpt-4o-mini",
    )
    .await;
    let asked = server
        .workspaces()
        .workspace_model(ADA, Interaction::Task)
        .letting_go(Quiet {
            first: Duration::from_millis(300),
            after_silence: Duration::from_millis(900),
        });
    let mut said = Vec::new();
    for _ in 0..2 {
        let due = tilo_clock::now()
            .checked_add(Duration::from_secs(5))
            .unwrap();
        let refused = asked
            .answer(&asking("When?", Vec::new()), due, &mut |_| {})
            .await
            .unwrap_err();
        let ModelRefusal::Provider(error) = refused else {
            panic!("the provider's own failure: {refused:?}");
        };
        assert_eq!(error.kind, ErrorKind::TimedOut);
        said.push(error.message);
    }
    assert_eq!(
        said,
        [
            "The model's provider sent nothing for 0.3 s",
            "The model's provider sent nothing for 0.9 s"
        ]
    );
}
