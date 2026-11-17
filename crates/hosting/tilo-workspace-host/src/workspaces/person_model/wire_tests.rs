//! A person's model asked for real: the host's own provider client against
//! servers that speak each provider's wire. What a person keeps in their
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

const ADA: [u8; 32] = [7; 32];
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
/// with that provider linked as Ada's own: its address, its key in her vault
/// and `model` as what her work runs on.
async fn linked(
    (name, protocol): (&str, ProviderProtocol),
    endpoint: &str,
    model: &str,
) -> (tempfile::TempDir, TestHost) {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    relay.workspaces().call_private_model_addresses(true);
    relay
        .workspaces()
        .link_model_for(
            &ADA,
            &Linking {
                name,
                protocol,
                endpoint,
                key: Some(KEY),
                models: &[model],
            },
            true,
        )
        .unwrap();
    (dir, relay)
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

/// An OpenAI-compatible provider is asked with a bearer key at its chat
/// completions, and what it streams arrives as it comes: the words, then the
/// whole answer with its tokens and what they cost at the model's price.
#[tokio::test]
async fn an_openai_compatible_provider_streams_its_answer_and_its_cost() {
    let model = "gpt-4o-mini";
    let served = Responder::start(vec![openai_stream(&[
        chunk(
            model,
            &json!({"role": "assistant", "content": "Good "}),
            &Value::Null,
        ),
        chunk(model, &json!({"content": "morning"}), &Value::Null),
        chunk(model, &json!({}), &json!("stop")),
        usage(model, 1_000, 500),
    ])]);
    let (_dir, relay) = linked(
        ("A provider", OPENAI),
        &format!("{}/v1", served.url()),
        model,
    )
    .await;
    let asked = relay.workspaces().person_model(ADA, Interaction::Request);

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

    let request = served.next_request();
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

/// A tool call an OpenAI-compatible provider streams comes back whole, with
/// its arguments as the provider wrote them; what streamed of it is for
/// showing only.
#[tokio::test]
async fn an_openai_compatible_providers_tool_call_comes_back_whole() {
    let model = "gpt-4o-mini";
    let served = Responder::start(vec![openai_stream(&[
        calling(model, None),
        chunk(model, &json!({}), &json!("tool_calls")),
        usage(model, 2_000, 100),
    ])]);
    let (_dir, relay) = linked(
        ("A provider", OPENAI),
        &format!("{}/v1", served.url()),
        model,
    )
    .await;

    let mut deltas = Vec::new();
    let calling = relay
        .workspaces()
        .person_model(ADA, Interaction::Request)
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
    let sent = body_of(&served.next_request());
    assert_eq!(sent["tools"][0]["function"]["name"], "lookup");
}

/// Anthropic is asked at its Messages API with the key in its own header,
/// and its stream of typed events arrives as words, a tool call and usage.
#[tokio::test]
async fn anthropic_is_asked_at_its_messages_api() {
    let model = "claude-sonnet-4-5";
    let served = Responder::start(vec![anthropic_stream(&[
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
    let (_dir, relay) = linked(
        ("Anthropic", ProviderProtocol::Anthropic),
        &served.url(),
        model,
    )
    .await;

    let mut stream = StreamState::default();
    let answer = relay
        .workspaces()
        .person_model(ADA, Interaction::Subtask)
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

    let request = served.next_request();
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
        let served = Responder::start(vec![anthropic_stream(&anthropic_calling(model))]);
        let (_dir, relay) = linked(
            ("Anthropic", ProviderProtocol::Anthropic),
            &served.url(),
            model,
        )
        .await;
        let mut request = asking("Look it up", vec![lookup()]);
        request.tool_choice = ToolChoice::Required;
        let answer = relay
            .workspaces()
            .person_model(ADA, Interaction::Task)
            .answer(&request, soon(), &mut |_| {})
            .await
            .unwrap();
        assert_eq!(answer.response.tool_calls().unwrap().len(), 1, "{model}");
        let sent = body_of(&served.next_request());
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
    let served = Responder::start(vec![
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
    let endpoint = format!("{}/v1beta/openai", served.url());
    let (_dir, relay) = linked(("Google", OPENAI), &endpoint, model).await;
    let asked = relay.workspaces().person_model(ADA, Interaction::Request);

    let mut request = asking("How many?", vec![lookup()]);
    let first = asked.answer(&request, soon(), &mut |_| {}).await.unwrap();
    let call = first.response.tool_calls().unwrap()[0].clone();
    assert_eq!(call.extra_content, Some(signature.clone()));
    assert!(first.response.cost_micros > 0);
    let sent = served.next_request();
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
    let sent = body_of(&served.next_request());
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
    let served = Responder::start(vec![
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
    let (_dir, relay) = linked(
        ("A provider", OPENAI),
        &format!("{}/v1", served.url()),
        model,
    )
    .await;
    let asked = relay.workspaces().person_model(ADA, Interaction::Task);

    let answer = asked
        .answer(&asking("When?", Vec::new()), soon(), &mut |_| {})
        .await
        .unwrap();
    assert_eq!(answer.text(), "Now.");
    assert_eq!(served.drain_requests().len(), 3, "asked until it answered");

    let refused = asked
        .answer(&asking("Again?", Vec::new()), soon(), &mut |_| {})
        .await
        .unwrap_err();
    assert_eq!(
        refused,
        ModelRefusal::Provider(ModelError::new(ErrorKind::Authentication))
    );
    assert!(!format!("{refused:?} {refused}").contains(KEY));
}

/// A call that is let go of while its answer still streams ends the request
/// at the provider: nothing nobody reads is paid for to its end.
#[tokio::test]
async fn a_call_let_go_of_ends_the_providers_request() {
    let (closed, was_closed) = std::sync::mpsc::channel();
    let closed = std::sync::Mutex::new(closed);
    let served = Responder::serve(move |peer| {
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
    let (_dir, relay) = linked(
        ("A provider", OPENAI),
        &format!("{}/v1", served.url()),
        "gpt-4o-mini",
    )
    .await;
    let asked = relay.workspaces().person_model(ADA, Interaction::Request);

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
    let served = Responder::silent();
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    for endpoint in [
        format!("{}/v1", served.url()),
        format!("https://127.0.0.1:{}/v1", served.port()),
        // The address a cloud machine reads its own credentials from.
        "https://169.254.169.254/v1".to_owned(),
        // A carrier-grade private range and a mapped private address.
        "https://100.64.0.1/v1".to_owned(),
        "https://[::ffff:10.0.0.1]/v1".to_owned(),
    ] {
        let person = *blake3::hash(endpoint.as_bytes()).as_bytes();
        workspaces
            .link_model_for(
                &person,
                &Linking {
                    name: "Inside",
                    protocol: OPENAI,
                    endpoint: &endpoint,
                    key: Some(KEY),
                    models: &["a-model"],
                },
                true,
            )
            .unwrap();
        let refused = workspaces
            .person_model(person, Interaction::Request)
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
    served.assert_untouched();
}
