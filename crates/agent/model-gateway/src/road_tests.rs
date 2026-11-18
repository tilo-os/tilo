//! The road a request takes to its model and the provider's cache along it,
//! asserted on the request the gateway actually sends ([`outbound_body`]).
use super::*;
use serde_json::{Value, json};

const OPENROUTER: &str = "https://openrouter.ai/api/v1";
const OPENAI: &str = "https://api.openai.com/v1";
const ELSEWHERE: &str = "https://models.example.test/api/v1";

fn at(endpoint: &str) -> ProviderProfile {
    ProviderProfile {
        endpoint: endpoint.into(),
        ..ProviderProfile::default()
    }
}

/// One step of a turn as the chat wire writes it: the instruction the turn
/// opens with, what the person said, and a tool.
fn step(instruction: &str, said: &str) -> Value {
    json!({
        "model": "named-by-the-caller",
        "messages": [
            {"role": "system", "content": [{"type": "text", "text": instruction}]},
            {"role": "user", "content": said},
        ],
        "tools": [{"type": "function", "function": {
            "name": "look", "parameters": {"type": "object"},
        }}],
        "tool_choice": "auto",
        "max_tokens": 4096,
    })
}

/// The same step as the Responses wire writes it.
fn responses_step(instruction: &str, said: &str) -> Value {
    json!({
        "model": "named-by-the-caller",
        "instructions": instruction,
        "input": [{"type": "message", "role": "user",
            "content": [{"type": "input_text", "text": said}]}],
        "tools": [{"type": "function", "name": "look", "parameters": {"type": "object"}}],
        "tool_choice": "auto",
        "max_output_tokens": 4096,
    })
}

fn sent(profile: &ProviderProfile, model: &str, body: Value) -> Value {
    outbound_body(profile, model, body, true, false).unwrap()
}

const INSTRUCTION: &str = "You are Ada's assistant. Your answer is posted in `chat/main`.";

tilo_test_env::scenario! {
    /// What a provider is asked beside the request itself, and where nothing is.
    fn a_request_takes_the_fastest_road_to_the_same_model_and_keeps_its_cache;

    phase a_provider_with_several_roads_to_one_model_is_asked_for_the_one_that_answers_first;
    {
        let body = sent(&at(OPENROUTER), "vendor/frontier", step(INSTRUCTION, "Hello"));
        assert_eq!(body["provider"], json!({"sort": "latency"}), "{body}");
        // The same model answers: its id goes as it is, with no suffix and
        // no second choice beside it.
        assert_eq!(body["model"], "vendor/frontier");
        assert!(body.get("models").is_none() && body.get("service_tier").is_none(), "{body}");

        // What a caller asked of the road itself stays as it asked it.
        let mut own = step(INSTRUCTION, "Hello");
        own["provider"] = json!({"only": ["one-place"]});
        let body = sent(&at(OPENROUTER), "vendor/frontier", own);
        assert_eq!(body["provider"], json!({"only": ["one-place"]}));

        // A person switches it off as they switch off any field.
        let mut quiet = at(OPENROUTER);
        quiet.omit_params = vec!["provider".into()];
        let body = sent(&quiet, "vendor/frontier", step(INSTRUCTION, "Hello"));
        assert!(body.get("provider").is_none(), "{body}");

        // A provider with one road is asked nothing about roads.
        for endpoint in [OPENAI, ELSEWHERE] {
            let body = sent(&at(endpoint), "frontier", step(INSTRUCTION, "Hello"));
            assert!(body.get("provider").is_none(), "{endpoint}: {body}");
        }
    }

    phase a_conversation_keeps_one_key_from_its_first_call_and_from_turn_to_turn;
    {
        let key = |body: &Value, field: &str| body[field].as_str().map(str::to_owned);
        // A turn's first call, a later step of it and the next turn's first
        // call open with the same instruction: one key.
        let first = sent(&at(OPENROUTER), "vendor/frontier", step(INSTRUCTION, "Hello"));
        let next_turn = sent(&at(OPENROUTER), "vendor/frontier", step(INSTRUCTION, "And now?"));
        let kept = key(&first, "session_id").expect("a key for the conversation");
        assert_eq!(key(&next_turn, "session_id"), Some(kept.clone()));
        assert!(kept.len() <= 256 && !kept.contains("Ada"), "{kept}");
        // Another conversation has another.
        let other = sent(
            &at(OPENROUTER),
            "vendor/frontier",
            step("You are Ada's assistant. Your answer is posted in `chat/other`.", "Hello"),
        );
        assert_ne!(key(&other, "session_id"), Some(kept.clone()));
        assert!(first.get("prompt_cache_key").is_none(), "{first}");

        // OpenAI's own API finds its cache by a key of its own name, on
        // both of its wires.
        let chat = sent(&at(OPENAI), "frontier", step(INSTRUCTION, "Hello"));
        assert_eq!(key(&chat, "prompt_cache_key"), Some(kept.clone()), "{chat}");
        let responses = outbound_body(
            &at(OPENAI),
            "frontier",
            responses_step(INSTRUCTION, "Hello"),
            true,
            true,
        )
        .unwrap();
        assert_eq!(key(&responses, "prompt_cache_key"), Some(kept), "{responses}");
        assert!(chat.get("session_id").is_none() && chat.get("provider").is_none(), "{chat}");

        // A provider that documents no such key is sent none.
        let body = sent(&at(ELSEWHERE), "frontier", step(INSTRUCTION, "Hello"));
        assert!(
            body.get("session_id").is_none() && body.get("prompt_cache_key").is_none(),
            "{body}"
        );
    }

    phase a_model_that_caches_only_what_is_marked_gets_its_instruction_marked;
    {
        let marked = json!({"type": "ephemeral"});
        let body = sent(&at(OPENROUTER), "anthropic/claude-x", step(INSTRUCTION, "Hello"));
        // The conversation as it grows, and the instruction with the tools
        // before it, which the next turn's first call opens with again.
        assert_eq!(body["cache_control"], marked, "{body}");
        let instruction = &body["messages"][0]["content"];
        assert_eq!(instruction[0]["text"], INSTRUCTION);
        assert_eq!(instruction[0]["cache_control"], marked, "{body}");
        assert_eq!(body["messages"][1], json!({"role": "user", "content": "Hello"}));

        // An instruction written as plain words is marked as one part.
        let mut plain = step(INSTRUCTION, "Hello");
        plain["messages"][0]["content"] = json!(INSTRUCTION);
        let body = sent(&at(OPENROUTER), "anthropic/claude-x", plain);
        assert_eq!(
            body["messages"][0]["content"],
            json!([{"type": "text", "text": INSTRUCTION, "cache_control": marked}])
        );

        // A model whose provider caches by itself is sent no mark, on this
        // road or any other.
        for (endpoint, model) in [
            (OPENROUTER, "vendor/frontier"),
            (OPENAI, "frontier"),
            (ELSEWHERE, "anthropic/claude-x"),
        ] {
            let body = sent(&at(endpoint), model, step(INSTRUCTION, "Hello"));
            assert!(!body.to_string().contains("cache_control"), "{endpoint} {model}: {body}");
        }
    }
}

/// An answer in words as an OpenAI-compatible provider streams one.
fn answering(text: &str) -> test_http::Reply {
    use std::fmt::Write as _;
    let chunk = |delta: Value, finish: Value| {
        json!({"id": "reply", "object": "chat.completion.chunk", "created": 1,
            "model": "vendor/frontier",
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
    };
    let usage = json!({"id": "reply", "object": "chat.completion.chunk", "created": 1,
        "model": "vendor/frontier", "choices": [],
        "usage": {"prompt_tokens": 10, "completion_tokens": 1, "total_tokens": 11}});
    let mut body = String::new();
    for event in [
        chunk(json!({"role": "assistant", "content": text}), Value::Null),
        chunk(json!({}), json!("stop")),
        usage,
    ] {
        write!(body, "data: {event}\n\n").unwrap();
    }
    body.push_str("data: [DONE]\n\n");
    test_http::Reply::status(200)
        .header("Content-Type", "text/event-stream")
        .body(body)
}

/// One step of a turn asked of `profile` through the path a person's own
/// provider is asked on.
async fn asked(
    client: &reqwest::Client,
    profile: &ProviderProfile,
) -> Result<agent_provider_config::protocol::ModelResponse, String> {
    use agent_provider_config::protocol as neutral;
    let request = neutral::ModelRequest {
        messages: vec![
            neutral::Message::text(neutral::Role::System, INSTRUCTION),
            neutral::Message::text(neutral::Role::User, "Hello"),
        ],
        tools: vec![neutral::ToolDefinition {
            name: "look".into(),
            description: "Look a thing up".into(),
            input_schema: json!({"type": "object"}),
        }],
        tool_choice: neutral::ToolChoice::Auto,
        max_output_tokens: 4096,
    };
    let call = PlanCall {
        profile_id: "theirs",
        model_id: "vendor/frontier",
        timeout: tilo_test_env::hang_bound(),
        stream: true,
        deltas: true,
        trace: None,
    };
    rig_adapter::complete_with(
        client,
        &call,
        (profile, Some(Zeroizing::new("made-up-key".to_owned()))),
        rig_adapter::neutral_request(request).map_err(|error| error.message)?,
        &mut |_| true,
        (
            PlanTiming::start(),
            rig_adapter::Carry {
                resend: true,
                callers_deadline: true,
            },
        ),
    )
    .await
    .map_err(|error| error.message)
}

/// A provider that refuses what the gateway added is asked once more without
/// it, its answer is the call's, and it is not sent it again. Nothing but
/// what was added differs between the two asks.
#[tokio::test(flavor = "current_thread")]
async fn a_field_the_provider_refuses_is_dropped_and_the_call_asked_again_once() {
    let stand_in = test_http::Responder::start(vec![
        test_http::Reply::status(400)
            .header("Content-Type", "application/json")
            .body(r#"{"error":{"message":"Unrecognized request argument supplied: provider"}}"#),
        answering("done"),
        answering("done again"),
    ]);
    // The stand-in answers under the provider's own name, so it is asked
    // what that provider is asked.
    let client = provider_builder()
        .resolve(
            "openrouter.ai",
            std::net::SocketAddr::from(([127, 0, 0, 1], stand_in.port())),
        )
        .build()
        .unwrap();
    let profile = at(&format!("http://openrouter.ai:{}/api/v1", stand_in.port()));
    let sent = |stand_in: &test_http::Responder| -> Vec<Value> {
        stand_in
            .drain_requests()
            .iter()
            .map(|request| serde_json::from_slice(&request.body).unwrap())
            .collect()
    };

    let answer = asked(&client, &profile).await.unwrap();
    assert_eq!(answer.usage.input_tokens, 10);
    let seen = sent(&stand_in);
    assert_eq!(seen.len(), 2, "asked again once: {seen:?}");
    // The whole of the first ask, by name: the step and the two fields
    // this provider is asked beside it.
    let mut names: Vec<&str> = seen[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "max_tokens",
            "messages",
            "model",
            "provider",
            "session_id",
            "stream",
            "stream_options",
            "tool_choice",
            "tools"
        ]
    );
    assert_eq!(seen[0]["provider"], json!({"sort": "latency"}));
    assert_eq!(seen[0]["model"], "vendor/frontier");
    let mut without = seen[0].clone();
    for field in ["provider", "session_id"] {
        without.as_object_mut().unwrap().remove(field);
    }
    assert_eq!(seen[1], without);

    // The next call is not sent what was refused.
    asked(&client, &profile).await.unwrap();
    let seen = sent(&stand_in);
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(
        seen[0].get("provider").is_none() && seen[0].get("session_id").is_none(),
        "{}",
        seen[0]
    );
}

/// A provider that takes what the gateway added is asked once, and a
/// refusal that is not about a field (a key, a quota) is not asked again.
#[tokio::test(flavor = "current_thread")]
async fn a_provider_that_takes_what_is_added_is_asked_once() {
    let stand_in = test_http::Responder::start(vec![
        answering("done"),
        test_http::Reply::status(401)
            .header("Content-Type", "application/json")
            .body(r#"{"error":{"message":"no such key"}}"#),
    ]);
    let client = provider_builder()
        .resolve(
            "openrouter.ai",
            std::net::SocketAddr::from(([127, 0, 0, 1], stand_in.port())),
        )
        .build()
        .unwrap();
    let profile = at(&format!("http://openrouter.ai:{}/api/v1", stand_in.port()));
    asked(&client, &profile).await.unwrap();
    assert!(asked(&client, &profile).await.is_err());
    let seen = stand_in.drain_requests();
    assert_eq!(seen.len(), 2, "each asked once");
    for request in &seen {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["provider"], json!({"sort": "latency"}), "{body}");
    }
}

const GOOGLE: &str = "https://generativelanguage.googleapis.com/v1beta/openai";

/// What Google's endpoint is asked beside a step that streams: its thought.
fn its_thought() -> Value {
    json!({"google": {"thinking_config": {"include_thoughts": true}}})
}

/// A step's answer as Google's endpoint streams one it was asked the
/// thought of: the thought in the content, marked, then the words.
fn thinking_then(thought: &str, text: &str) -> test_http::Reply {
    use std::fmt::Write as _;
    let chunk = |delta: Value, finish: Value| {
        json!({"id": "reply", "object": "chat.completion.chunk", "created": 1,
            "model": "vendor/frontier",
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
    };
    let marked = json!({"role": "assistant", "content": thought,
        "extra_content": {"google": {"thought": true}}});
    let mut body = String::new();
    for event in [
        chunk(marked, Value::Null),
        chunk(json!({"content": text}), Value::Null),
        chunk(json!({}), json!("stop")),
    ] {
        write!(body, "data: {event}\n\n").unwrap();
    }
    body.push_str("data: [DONE]\n\n");
    test_http::Reply::status(200)
        .header("Content-Type", "text/event-stream")
        .body(body)
}

/// A wait shows the model thinking: Google's endpoint thinks before every
/// step and returns the answer alone unless it is asked for its thought.
/// It is asked where the answer streams. The model and how hard it thinks
/// stay as they are.
#[test]
fn googles_endpoint_is_asked_for_its_thought_where_the_answer_streams() {
    let body = sent(&at(GOOGLE), "models/frontier", step(INSTRUCTION, "Hello"));
    assert_eq!(body["extra_body"], its_thought(), "{body}");
    assert_eq!(body["model"], "models/frontier");
    assert!(body.get("reasoning_effort").is_none(), "{body}");

    // An answer that comes whole shows nothing while it is made: not asked.
    let whole = step(INSTRUCTION, "Hello");
    let body = outbound_body(&at(GOOGLE), "models/frontier", whole, false, false).unwrap();
    assert!(body.get("extra_body").is_none(), "{body}");

    // The level its person chose goes beside it, as they chose it.
    let mut levelled = at(GOOGLE);
    levelled.reasoning = Some(agent_provider_config::ReasoningSetting {
        param: agent_provider_config::ReasoningParam::ReasoningEffort,
        effort: "low".into(),
    });
    let body = sent(&levelled, "models/frontier", step(INSTRUCTION, "Hello"));
    assert_eq!(body["reasoning_effort"], "low", "{body}");
    assert_eq!(body["extra_body"], its_thought(), "{body}");

    // What a caller asked of Google itself stays as it asked it.
    let kept = json!({"google": {"cached_content": "cachedContents/kept"}});
    let mut own = step(INSTRUCTION, "Hello");
    own["extra_body"] = kept.clone();
    let body = sent(&at(GOOGLE), "models/frontier", own);
    assert_eq!(body["extra_body"], kept, "{body}");

    // A person switches it off as they switch off any field.
    let mut quiet = at(GOOGLE);
    quiet.omit_params = vec!["extra_body".into()];
    let body = sent(&quiet, "models/frontier", step(INSTRUCTION, "Hello"));
    assert!(body.get("extra_body").is_none(), "{body}");

    // No other provider is asked it.
    for endpoint in [OPENROUTER, OPENAI, ELSEWHERE] {
        let body = sent(&at(endpoint), "frontier", step(INSTRUCTION, "Hello"));
        assert!(body.get("extra_body").is_none(), "{endpoint}: {body}");
    }
}

/// Asked for its thought, Google's endpoint is answered with one, and the
/// turn keeps it as a thought before the answer's words. Where the endpoint
/// refuses the asking, the step is asked once more without it, answers, and
/// is not asked for a thought again.
#[tokio::test(flavor = "current_thread")]
async fn googles_thought_is_kept_as_one_and_a_refused_asking_is_asked_again_once() {
    let stand_in = test_http::Responder::start(vec![
        thinking_then("THE THOUGHT.", "done"),
        test_http::Reply::status(400)
            .header("Content-Type", "application/json")
            .body(r#"{"error":{"message":"Unknown name \"extra_body\": Cannot find field."}}"#),
        answering("done"),
        answering("done again"),
    ]);
    // The stand-in answers under Google's own name, so it is asked what
    // Google's endpoint is asked.
    let name = "generativelanguage.googleapis.com";
    let client = provider_builder()
        .resolve(
            name,
            std::net::SocketAddr::from(([127, 0, 0, 1], stand_in.port())),
        )
        .build()
        .unwrap();
    let profile = at(&format!("http://{name}:{}/v1beta/openai", stand_in.port()));
    let sent = |stand_in: &test_http::Responder| -> Vec<Value> {
        stand_in
            .drain_requests()
            .iter()
            .map(|request| serde_json::from_slice(&request.body).unwrap())
            .collect()
    };

    let answer = asked(&client, &profile).await.unwrap();
    let seen = sent(&stand_in);
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0]["extra_body"], its_thought(), "{}", seen[0]);
    assert_eq!(
        json!(answer.message.content),
        json!([
            {"type": "reasoning", "text": "THE THOUGHT."},
            {"type": "text", "text": "done"},
        ])
    );

    // Refused: asked again once, and nothing but the asking differs.
    let answer = asked(&client, &profile).await.unwrap();
    assert_eq!(answer.usage.input_tokens, 10);
    let seen = sent(&stand_in);
    assert_eq!(seen.len(), 2, "asked again once: {seen:?}");
    assert_eq!(seen[0]["extra_body"], its_thought(), "{}", seen[0]);
    let mut without = seen[0].clone();
    without.as_object_mut().unwrap().remove("extra_body");
    assert_eq!(seen[1], without);

    // The next step is not asked for what was refused.
    asked(&client, &profile).await.unwrap();
    let seen = sent(&stand_in);
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(seen[0].get("extra_body").is_none(), "{}", seen[0]);
}
