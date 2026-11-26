//! A thought Google's endpoint writes into the content of the answer it
//! belongs to is read as the answer's thought, never as its words: as it
//! streams, in the turn that is kept, and in what the next ask sends back.

use serde_json::{Value, json};

use super::{Ask, Delta, Stream, body, response};
use crate::protocol::{
    Content, Message, ModelRequest, ModelResponse, Role, ToolChoice, ToolDefinition,
};
use crate::{ProviderProfile, ProviderProtocol, presets};

/// What a thought says in every form below.
const THOUGHT: &str = "The person wants the count; I will look it up.";
/// What the answer says.
const ANSWER: &str = "There are two.";

/// Google's own endpoint, as the Gemini preset names it.
fn googles() -> ProviderProfile {
    presets::Preset::GEMINI.profile()
}

/// Another endpoint speaking the same protocol.
fn another() -> ProviderProfile {
    let mut profile = ProviderProfile::hosted(ProviderProtocol::OpenAiCompatible);
    profile.endpoint = "https://models.example.test/v1".into();
    profile
}

fn request() -> ModelRequest {
    ModelRequest {
        messages: vec![Message::text(Role::User, "How many are there?")],
        tools: vec![ToolDefinition {
            name: "lookup".into(),
            description: "Look up a count".into(),
            input_schema: json!({"type": "object"}),
        }],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: 256,
    }
}

/// A Gemini stream as Google's endpoint answers a streamed step it was asked the thought of:
/// the thought between tags at the head of the content, the answer after it, and a signed call.
fn recorded_gemini_stream() -> String {
    let chunk = |delta: Value, finish: Value| {
        json!({"id": "reply", "object": "chat.completion.chunk", "model": "gemini-2.5-flash",
               "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
    };
    let chunks = [
        chunk(
            json!({"role": "assistant", "content": "<thou"}),
            Value::Null,
        ),
        chunk(
            json!({"content": "ght>The person wants the count; "}),
            Value::Null,
        ),
        chunk(json!({"content": "I will look it up.</tho"}), Value::Null),
        chunk(json!({"content": "ught>There are two."}), Value::Null),
        chunk(
            json!({"tool_calls": [{"index": 0, "id": "call_1", "type": "function",
                "function": {"name": "lookup", "arguments": "{}"},
                "extra_content": {"google": {"thought_signature": "signed-thought"}}}]}),
            Value::Null,
        ),
        chunk(json!({}), json!("tool_calls")),
    ];
    events(&chunks)
}

/// `chunks` as a stream of server-sent events, ended as chat streams end.
fn events(chunks: &[Value]) -> String {
    let mut stream = String::new();
    for chunk in chunks {
        stream.push_str("data: ");
        stream.push_str(&chunk.to_string());
        stream.push_str("\n\n");
    }
    stream.push_str("data: [DONE]\n\n");
    stream
}

/// `stream` read for `profile` in pieces of `cut` bytes: the turn, and the deltas shown.
fn read(profile: &ProviderProfile, stream: &str, cut: usize) -> (ModelResponse, Vec<Delta>) {
    let mut reader = Stream::of(profile);
    let mut deltas = Vec::new();
    for piece in stream.as_bytes().chunks(cut.max(1)) {
        reader.push(piece, &mut deltas).unwrap();
    }
    let turn = reader.finish(&request(), &mut deltas).unwrap();
    (turn, deltas)
}

/// What the deltas showed as the thought, and as the answer's words.
fn shown(deltas: &[Delta]) -> (String, String) {
    let (mut thought, mut words) = (String::new(), String::new());
    for delta in deltas {
        match delta {
            Delta::Reasoning(text) => thought.push_str(text),
            Delta::Text(text) => words.push_str(text),
            _ => {}
        }
    }
    (thought, words)
}

/// The thought and the words a turn keeps, in order.
fn kept(turn: &ModelResponse) -> Vec<(bool, &str)> {
    turn.message
        .content
        .iter()
        .filter_map(|content| match content {
            Content::Reasoning { text, .. } => Some((true, text.as_str())),
            Content::Text { text } => Some((false, text.as_str())),
            _ => None,
        })
        .collect()
}

/// The assistant's message as the next ask sends `turn` back.
fn sent_back(turn: &ModelResponse) -> Value {
    let mut next = request();
    next.messages.push(turn.message.clone());
    next.messages.push(Message {
        role: Role::Tool,
        content: vec![Content::ToolResult(crate::protocol::ToolResult {
            call_id: "call_1".into(),
            content: "2".into(),
            is_error: false,
        })],
    });
    let sent = body(&googles(), "gemini-2.5-flash", &next, Ask::default()).unwrap();
    sent["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "assistant")
        .cloned()
        .unwrap()
}

/// A recorded Gemini stream, cut anywhere, shows its thought as the thought and its answer as
/// the words; the turn keeps them apart, and the next ask sends the answer and the call's
/// signature back but not the thought.
#[test]
fn a_gemini_stream_keeps_its_thought_apart_from_its_answer() {
    let stream = recorded_gemini_stream();
    for cut in 1..stream.len() {
        let (turn, deltas) = read(&googles(), &stream, cut);
        assert_eq!(
            shown(&deltas),
            (THOUGHT.to_owned(), ANSWER.to_owned()),
            "cut at {cut}"
        );
        assert_eq!(
            kept(&turn),
            [(true, THOUGHT), (false, ANSWER)],
            "cut at {cut}"
        );
        let back = sent_back(&turn);
        assert_eq!(back["content"], ANSWER, "cut at {cut}");
        assert!(!back.to_string().contains("look it up"), "{back}");
        assert_eq!(
            back["tool_calls"][0]["extra_content"],
            json!({"google": {"thought_signature": "signed-thought"}}),
            "the call's signature goes back"
        );
    }
}

/// A delta its provider marks as a thought, and a content part marked as one as Google's own
/// interface marks it, are the thought at any endpoint.
#[test]
fn a_marked_delta_or_part_is_the_thought_at_any_endpoint() {
    let stream = [
        json!({"choices": [{"index": 0, "delta": {"content": THOUGHT,
            "extra_content": {"google": {"thought": true}}}, "finish_reason": null}]}),
        json!({"choices": [{"index": 0, "delta": {"content": [
            {"type": "text", "text": " Counting.", "thought": true},
            {"type": "text", "text": ANSWER}]}, "finish_reason": null}]}),
        json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}),
    ];
    let stream = events(&stream);
    for profile in [googles(), another()] {
        let (turn, deltas) = read(&profile, &stream, usize::MAX);
        let thought = format!("{THOUGHT} Counting.");
        assert_eq!(shown(&deltas), (thought.clone(), ANSWER.to_owned()));
        assert_eq!(kept(&turn), [(true, thought.as_str()), (false, ANSWER)]);
    }
}

/// A whole answer from Google's endpoint keeps its tagged thought apart too.
#[test]
fn a_whole_gemini_answer_keeps_its_thought_apart() {
    let answer = json!({"choices": [{"index": 0, "finish_reason": "stop", "message": {
        "role": "assistant", "content": format!("<thought>{THOUGHT}</thought>{ANSWER}")}}]});
    let turn = response(&googles(), &request(), &answer.to_string()).unwrap();
    assert_eq!(kept(&turn), [(true, THOUGHT), (false, ANSWER)]);
}

/// Only a thought is read out of an answer: words that merely begin like the tag stay words at
/// Google's endpoint, and the tags themselves stay words at any other.
#[test]
fn words_that_only_look_like_a_thought_stay_words() {
    let streamed = |content: &str| {
        format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({"choices": [{"index": 0, "delta": {"content": content}, "finish_reason": null}]}),
            json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}),
        )
    };
    let lookalike = "<thoughtful> is a word.";
    let (turn, deltas) = read(&googles(), &streamed(lookalike), 3);
    assert_eq!(shown(&deltas), (String::new(), lookalike.to_owned()));
    assert_eq!(kept(&turn), [(false, lookalike)]);
    let tagged = format!("<thought>{THOUGHT}</thought>{ANSWER}");
    let (turn, _) = read(&another(), &streamed(&tagged), 4);
    assert_eq!(kept(&turn), [(false, tagged.as_str())]);
}

/// A streamed step at Google's endpoint asks for its thought; a whole one, another endpoint,
/// and a caller that asked Google something itself are not asked.
#[test]
fn googles_endpoint_is_asked_for_its_thought_where_the_answer_streams() {
    let asked = |profile: &ProviderProfile, streaming: bool, extra| {
        let ask = Ask { streaming, extra };
        body(profile, "gemini-2.5-flash", &request(), ask).unwrap()
    };
    assert_eq!(
        asked(&googles(), true, None)["extra_body"],
        json!({"google": {"thinking_config": {"include_thoughts": true}}})
    );
    assert!(asked(&googles(), false, None).get("extra_body").is_none());
    assert!(asked(&another(), true, None).get("extra_body").is_none());
    let own: serde_json::Map<String, Value> =
        serde_json::from_value(json!({"extra_body": {"google": {"cached_content": "c1"}}}))
            .unwrap();
    assert_eq!(
        asked(&googles(), true, Some(&own))["extra_body"],
        json!({"google": {"cached_content": "c1"}})
    );
    let mut quiet = googles();
    quiet.omit_params = vec!["extra_body".into()];
    assert!(asked(&quiet, true, None).get("extra_body").is_none());
}
