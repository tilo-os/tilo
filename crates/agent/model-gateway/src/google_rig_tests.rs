use super::*;
use serde_json::json;
use test_http::{Reply, Responder};

/// The two turns a signed-in Google account is answered with: a tool call
/// whose signature comes back, then the answer to its result.
fn google_peer(streaming: bool) -> Responder {
    let extra = json!({"google":{"thought_signature":"opaque-signed-thought"}});
    let call = json!({"id":"call_1","type":"function","function":{"name":"lookup","arguments":"{\"count\":2}"},"extra_content":extra});
    let first = if streaming {
        let mut call = call;
        call["index"] = json!(0);
        let delta = json!({"id":"reply","object":"chat.completion.chunk","created":1,"model":"checked-model","choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[call]},"finish_reason":null}]});
        let finish = json!({"id":"reply","object":"chat.completion.chunk","created":1,"model":"checked-model","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]});
        Reply::status(200)
            .header("Content-Type", "text/event-stream")
            .body(format!(
                "data: {delta}\n\ndata: {finish}\n\ndata: [DONE]\n\n"
            ))
    } else {
        let reply = json!({"id":"reply","object":"chat.completion","created":1,"model":"checked-model","choices":[{"index":0,"finish_reason":"tool_calls","message":{"role":"assistant","tool_calls":[call]}}]});
        Reply::status(200)
            .header("Content-Type", "application/json")
            .body(reply.to_string())
    };
    let answer = json!({"id":"reply","object":"chat.completion","created":1,"model":"checked-model","choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"hello"}}]});
    Responder::start(vec![
        first,
        Reply::status(200)
            .header("Content-Type", "application/json")
            .body(answer.to_string()),
    ])
}

/// The two turns as the provider saw them: the account's headers, the model,
/// and the tool history the second turn carries.
fn google_turns(served: &Responder, streaming: bool) {
    for turn in 0..2 {
        let request = served.next_request();
        assert_eq!(request.method, "POST");
        assert_eq!(request.target, "/chat/completions");
        assert_eq!(
            request.header("authorization"),
            Some("Bearer minted-test-token")
        );
        assert_eq!(
            request.header("x-goog-user-project"),
            Some("personal-billing-project")
        );
        let body: serde_json::Value = serde_json::from_str(&request.body_text()).unwrap();
        assert_eq!(body["model"], "checked-model");
        if turn == 0 {
            if streaming {
                assert_eq!(body["stream"], true);
            }
            continue;
        }
        let messages = body["messages"].as_array().unwrap();
        let call = &messages
            .iter()
            .find(|message| message["role"] == "assistant")
            .unwrap()["tool_calls"][0];
        assert_eq!(call["id"], "call_1");
        assert_eq!(call["function"]["name"], "lookup");
        assert_eq!(call["function"]["arguments"], "{\"count\":2}");
        assert_eq!(
            call["extra_content"],
            json!({"google":{"thought_signature":"opaque-signed-thought"}})
        );
        let result = messages
            .iter()
            .find(|message| message["role"] == "tool")
            .unwrap();
        assert_eq!(result["tool_call_id"], "call_1");
        assert_eq!(result["content"], "found");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn google_billing_and_gemini_signatures_survive_unary_and_streamed_tool_history() {
    for streaming in [false, true] {
        let peer = google_peer(streaming);
        let mut profile =
            ProviderProfile::google_vertex("personal-billing-project", "global", "checked-model");
        profile.endpoint = peer.url();
        let client = provider_client().unwrap();
        let mut history = neutral::Conversation::new("instructions", "question");
        let tools = vec![neutral::ToolDefinition {
            name: "lookup".into(),
            description: "Look up a resource".into(),
            input_schema: json!({"type":"object","properties":{"count":{"type":"integer"}}}),
        }];
        let request = neutral_request(history.request(tools.clone(), 256)).unwrap();
        let first = if streaming {
            let transport = GatewayHttp {
                client: client.clone(),
                endpoint: provider_url(&profile),
                profile: profile.clone(),
                model: "checked-model".into(),
                streaming: true,
                api: None,
                target: crate::openai::Target::Standard,
                retry_refusals: false,
                stream_end: Arc::default(),
                profile_headers: Vec::new(),
                budget: tilo_test_env::hang_bound(),
                body: None,
                failure: Arc::default(),
                status: Arc::default(),
                unary_body: Arc::default(),
                signs: Arc::default(),
            };
            execute(
                transport,
                Some("minted-test-token"),
                request,
                Some((&mut |_| true, &mut PlanTiming::start(), false)),
            )
            .await
        } else {
            host_complete(
                &client,
                &profile,
                Some("minted-test-token"),
                "checked-model",
                request,
                tilo_test_env::hang_bound(),
                crate::openai::Target::Standard,
            )
            .await
        }
        .map_err(|failure| failure.why)
        .unwrap();
        let response = neutral_response(first).unwrap();
        let response: neutral::ModelResponse =
            serde_json::from_value(serde_json::to_value(response).unwrap()).unwrap();
        assert_eq!(
            response.tool_calls().unwrap()[0].extra_content,
            Some(json!({"google":{"thought_signature":"opaque-signed-thought"}}))
        );
        history
            .push(
                response,
                vec![neutral::ToolResult {
                    call_id: "call_1".into(),
                    content: "found".into(),
                    is_error: false,
                }],
            )
            .unwrap();
        host_complete(
            &client,
            &profile,
            Some("minted-test-token"),
            "checked-model",
            neutral_request(history.request(tools, 256)).unwrap(),
            tilo_test_env::hang_bound(),
            crate::openai::Target::Standard,
        )
        .await
        .map_err(|failure| failure.why)
        .unwrap();

        google_turns(&peer, streaming);
    }
}

/// What a thought says in every form below.
const THOUGHT: &str = "THE THOUGHT SUMMARY.";
/// What the answer says where a step answers in words.
const ANSWER: &str = "THE ANSWER.";
/// A key no provider knows.
const MADE_UP_KEY: &str = "made-up-key-of-a-test";

/// The Gemini API's endpoint as a person's settings name it.
fn googles_own() -> ProviderProfile {
    agent_provider_config::presets::Preset::GEMINI.profile()
}

/// Any other endpoint that speaks the same protocol.
fn another_providers() -> ProviderProfile {
    let mut profile = ProviderProfile::hosted(ProviderProtocol::OpenAiCompatible);
    profile.endpoint = "https://models.example.test/v1".into();
    profile
}

/// A transport shaped for `profile` whose requests reach `peer`: the profile
/// keeps its provider's own address, which is what says who is spoken to.
fn reaching(peer: &Responder, profile: &ProviderProfile, streaming: bool) -> GatewayHttp {
    let mut dialled = profile.clone();
    dialled.endpoint = format!("{}/v1", peer.url());
    GatewayHttp {
        client: provider_client().unwrap(),
        endpoint: provider_url(&dialled),
        profile: profile.clone(),
        model: "models/frontier-model".into(),
        streaming,
        api: None,
        target: crate::openai::Target::Standard,
        retry_refusals: false,
        stream_end: Arc::default(),
        profile_headers: Vec::new(),
        budget: tilo_test_env::hang_bound(),
        body: None,
        failure: Arc::default(),
        status: Arc::default(),
        unary_body: Arc::default(),
        signs: Arc::default(),
    }
}

/// One streamed piece of a step.
fn piece(delta: &serde_json::Value, finish: &serde_json::Value) -> String {
    format!(
        "data: {}\n\n",
        json!({"id":"reply","object":"chat.completion.chunk","created":1,"model":"frontier-model","choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
    )
}

/// A step streamed as `deltas` and ended as `finish` says.
fn streamed_as(deltas: &[serde_json::Value], finish: &str) -> Reply {
    let mut body: String = deltas
        .iter()
        .map(|delta| piece(delta, &json!(null)))
        .collect();
    body.push_str(&piece(&json!({}), &json!(finish)));
    body.push_str("data: [DONE]\n\n");
    Reply::status(200)
        .header("Content-Type", "text/event-stream")
        .body(body)
}

/// A step answered whole.
fn whole_as(message: &serde_json::Value) -> Reply {
    let reply = json!({"id":"reply","object":"chat.completion","created":1,"model":"frontier-model","choices":[{"index":0,"finish_reason":"stop","message":message}]});
    Reply::status(200)
        .header("Content-Type", "application/json")
        .body(reply.to_string())
}

/// The tool a step below may call.
fn lookup() -> Vec<neutral::ToolDefinition> {
    vec![neutral::ToolDefinition {
        name: "lookup".into(),
        description: "Look up a resource".into(),
        input_schema: json!({"type":"object","properties":{"count":{"type":"integer"}}}),
    }]
}

/// The call a step ends with, signed as Google signs one.
fn signed_call() -> serde_json::Value {
    json!({"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"lookup","arguments":"{\"count\":2}"},"extra_content":{"google":{"thought_signature":"opaque-signed-thought"}}}]})
}

/// What a step handed on while it streamed: its thought, and its words.
#[derive(Default)]
struct HandedOn {
    thought: String,
    words: String,
}

/// Ask `transport` one streamed step.
async fn streamed(
    transport: GatewayHttp,
    request: CompletionRequest,
) -> (HandedOn, CompletionResponse) {
    let mut handed = HandedOn::default();
    let mut emit = |frame: &Response| {
        if let (Some(kind), Some(payload)) = (frame.event.as_deref(), frame.output.as_deref()) {
            match crate::transport::decode_event(kind, payload) {
                Ok(Some(neutral::StreamEvent::ReasoningDelta { text })) => {
                    handed.thought.push_str(&text);
                }
                Ok(Some(neutral::StreamEvent::TextDelta { text })) => handed.words.push_str(&text),
                _ => {}
            }
        }
        true
    };
    let response = execute(
        transport,
        Some(MADE_UP_KEY),
        request,
        Some((&mut emit, &mut PlanTiming::start(), true)),
    )
    .await
    .map_err(|failure| failure.why)
    .unwrap();
    (handed, response)
}

/// What a step that thinks and then calls its tool left behind.
struct Left {
    handed: HandedOn,
    /// What the turn keeps of the step.
    kept: Vec<neutral::Content>,
    /// The message's content as the older plan path reads it.
    read: Option<serde_json::Value>,
    /// The assistant's message as the next ask sends it back.
    back: serde_json::Value,
}

/// A step at `profile` that thinks as `thought` streams it and then calls
/// its tool, and the step that follows the tool's answer.
async fn thinks_then_calls(profile: &ProviderProfile, thought: Vec<serde_json::Value>) -> Left {
    let mut deltas = thought;
    deltas.push(signed_call());
    let peer = Responder::start(vec![
        streamed_as(&deltas, "tool_calls"),
        whole_as(&json!({"role":"assistant","content":"hello"})),
    ]);
    let (first, second) = (
        reaching(&peer, profile, true),
        reaching(&peer, profile, false),
    );
    let mut history = neutral::Conversation::new("instructions", "question");
    let request = neutral_request(history.request(lookup(), 256)).unwrap();
    let (handed, response) = streamed(first, request).await;
    let read = response.raw.pointer("/choices/0/message/content").cloned();
    let answer = neutral_response(response).unwrap();
    let kept = answer.message.content.clone();
    let found = neutral::ToolResult {
        call_id: "call_1".into(),
        content: "found".into(),
        is_error: false,
    };
    history.push(answer, vec![found]).unwrap();
    let request = neutral_request(history.request(lookup(), 256)).unwrap();
    execute(second, Some(MADE_UP_KEY), request, None)
        .await
        .map_err(|failure| failure.why)
        .unwrap();
    let asked = peer.drain_requests();
    let sent: serde_json::Value = serde_json::from_str(&asked[1].body_text()).unwrap();
    let back = sent["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "assistant")
        .cloned()
        .unwrap();
    Left {
        handed,
        kept,
        read,
        back,
    }
}

/// `left` holds the thought as a thought and nowhere as the answer's words.
fn holds_the_thought_apart(form: &str, left: &Left) {
    assert_eq!(
        left.handed.thought, THOUGHT,
        "{form}: handed on as the thought"
    );
    assert_eq!(
        left.handed.words, "",
        "{form}: none of it handed on as the answer's words"
    );
    assert!(
        matches!(left.kept.first(), Some(neutral::Content::Reasoning { text, .. }) if text == THOUGHT),
        "{form}: kept as the thought: {:?}",
        left.kept
    );
    assert!(
        !left
            .kept
            .iter()
            .any(|content| matches!(content, neutral::Content::Text { .. })),
        "{form}: none of it kept as the reply's text: {:?}",
        left.kept
    );
    for (place, content) in [
        ("what the older plan path reads", left.read.as_ref()),
        (
            "the assistant's content in the next ask",
            left.back.get("content"),
        ),
    ] {
        assert!(
            content.is_none_or(|content| !content.to_string().contains("THOUGHT")),
            "{form}: {place} holds no thought: {content:?}"
        );
    }
    assert_eq!(
        left.back["tool_calls"][0]["extra_content"],
        json!({"google":{"thought_signature":"opaque-signed-thought"}}),
        "{form}: the call's signature still goes back"
    );
}

/// What a step that answers in words left: what it handed on, what the turn
/// keeps of it, and its content as the older plan path reads it.
type Said = (HandedOn, Vec<neutral::Content>, Option<serde_json::Value>);

/// A step at `profile` that `reply` answers, streamed or whole.
async fn answered(profile: &ProviderProfile, reply: Reply, streaming: bool) -> Said {
    let peer = Responder::start(vec![reply]);
    let transport = reaching(&peer, profile, streaming);
    let history = neutral::Conversation::new("instructions", "question");
    let request = neutral_request(history.request(Vec::new(), 256)).unwrap();
    let (handed, response) = if streaming {
        streamed(transport, request).await
    } else {
        let response = execute(transport, Some(MADE_UP_KEY), request, None)
            .await
            .map_err(|failure| failure.why)
            .unwrap();
        (HandedOn::default(), response)
    };
    let read = response.raw.pointer("/choices/0/message/content").cloned();
    let kept = neutral_response(response).unwrap().message.content;
    (handed, kept, read)
}

/// The thought and the words `kept` holds, in their order.
fn thought_and_words(kept: &[neutral::Content]) -> Vec<(bool, &str)> {
    kept.iter()
        .filter_map(|content| match content {
            neutral::Content::Reasoning { text, .. } => Some((true, text.as_str())),
            neutral::Content::Text { text } => Some((false, text.as_str())),
            _ => None,
        })
        .collect()
}

/// A thought is handed on and kept as a thought and is never the answer's
/// words: not as it streams, not in what the turn keeps, not in what the
/// older plan path reads, and not in what the next ask sends back as the
/// assistant's own content.
#[tokio::test(flavor = "current_thread")]
async fn a_thought_is_never_the_answers_words() {
    let mark = json!({"google":{"thought":true}});
    for (form, thought) in [
        (
            "in the field of its own",
            vec![
                json!({"role":"assistant","reasoning_content":"THE THOUGHT "}),
                json!({"reasoning_content":"SUMMARY."}),
            ],
        ),
        (
            "in the content, marked as a thought",
            vec![
                json!({"role":"assistant","content":"THE THOUGHT ","extra_content":mark}),
                json!({"content":"SUMMARY.","extra_content":mark}),
            ],
        ),
    ] {
        for profile in [googles_own(), another_providers()] {
            let left = thinks_then_calls(&profile, thought.clone()).await;
            holds_the_thought_apart(form, &left);
        }
    }
}

/// Google's own interface marks a thought on the part that holds it. Should
/// its other endpoint hand such parts on, a part marked as a thought, and
/// every part of a delta marked as one, is the thought and never the
/// answer's words.
#[tokio::test(flavor = "current_thread")]
async fn a_thought_in_a_part_of_the_content_is_never_the_answers_words() {
    let mark = json!({"google":{"thought":true}});
    for (form, thought) in [
        (
            "in a part marked as a thought",
            json!({"role":"assistant","content":[{"type":"text","text":THOUGHT,"thought":true}]}),
        ),
        (
            "in the parts of a delta marked as a thought",
            json!({"role":"assistant","content":[{"type":"text","text":THOUGHT}],"extra_content":mark}),
        ),
    ] {
        for profile in [googles_own(), another_providers()] {
            let left = thinks_then_calls(&profile, vec![thought.clone()]).await;
            holds_the_thought_apart(form, &left);
        }
    }
}

/// Google's endpoint may write a thought into the content it belongs to,
/// between tags at its head; its page names no form. The thought is read
/// out of the content whatever pieces it comes in, marked or not, and the
/// answer's words are what follows it.
#[tokio::test(flavor = "current_thread")]
async fn a_thought_between_tags_at_googles_endpoint_is_never_the_answers_words() {
    let mark = json!({"google":{"thought":true}});
    for (form, thought) in [
        (
            "between tags, cut inside them",
            vec![
                json!({"role":"assistant","content":"<thou"}),
                json!({"content":"ght>THE THOUGHT "}),
                json!({"content":"SUMMARY.</tho"}),
                json!({"content":"ught>"}),
            ],
        ),
        (
            "between tags and marked",
            vec![
                json!({"role":"assistant","content":"<thought>THE THOUGHT ","extra_content":mark}),
                json!({"content":"SUMMARY.","extra_content":mark}),
                json!({"content":"</thought>"}),
            ],
        ),
    ] {
        let left = thinks_then_calls(&googles_own(), thought).await;
        holds_the_thought_apart(form, &left);
    }
    let both = format!("<thought>{THOUGHT}</thought>{ANSWER}");
    for cut in 1..both.len() {
        let (before, after) = both.split_at(cut);
        let deltas = [
            json!({"role":"assistant","content":before}),
            json!({"content":after}),
        ];
        let (handed, kept, read) =
            answered(&googles_own(), streamed_as(&deltas, "stop"), true).await;
        assert_eq!(
            (handed.thought.as_str(), handed.words.as_str()),
            (THOUGHT, ANSWER),
            "cut at {cut}: handed on"
        );
        assert_eq!(
            thought_and_words(&kept),
            [(true, THOUGHT), (false, ANSWER)],
            "cut at {cut}: kept"
        );
        assert_eq!(
            read,
            Some(json!(ANSWER)),
            "cut at {cut}: the older plan path"
        );
    }
    // An answer that comes whole is read the same way. Its raw message stays
    // its provider's own body, tags and all: a thought is not to be asked
    // for where an answer does not stream.
    let whole = whole_as(&json!({"role":"assistant","content":both}));
    let (_, kept, _) = answered(&googles_own(), whole, false).await;
    assert_eq!(thought_and_words(&kept), [(true, THOUGHT), (false, ANSWER)]);
}

/// Only a thought is read out of an answer. Words that merely begin like
/// the tag stay the answer's words, and so do the tags themselves at any
/// endpoint but Google's.
#[tokio::test(flavor = "current_thread")]
async fn words_that_only_look_like_a_thought_stay_the_answers_words() {
    for pieces in [
        vec!["<t", "able> of things"],
        vec!["<tho"],
        vec!["  <", "3"],
    ] {
        let deltas: Vec<_> = pieces.iter().map(|part| json!({"content":part})).collect();
        let (handed, kept, read) =
            answered(&googles_own(), streamed_as(&deltas, "stop"), true).await;
        let words = pieces.concat();
        assert_eq!(
            (handed.thought.as_str(), handed.words.as_str()),
            ("", words.as_str())
        );
        assert_eq!(thought_and_words(&kept), [(false, words.as_str())]);
        assert_eq!(read, Some(json!(words)));
    }
    let tagged = format!("<thought>{THOUGHT}</thought>{ANSWER}");
    let deltas = [json!({"role":"assistant","content":tagged})];
    let (handed, kept, read) =
        answered(&another_providers(), streamed_as(&deltas, "stop"), true).await;
    assert_eq!(
        (handed.thought.as_str(), handed.words.as_str()),
        ("", tagged.as_str())
    );
    assert_eq!(thought_and_words(&kept), [(false, tagged.as_str())]);
    assert_eq!(read, Some(json!(tagged)));
}
