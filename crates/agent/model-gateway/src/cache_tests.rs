//! Anthropic prompt-cache breakpoints, asserted on the request the gateway
//! actually sends ([`outbound_body`]).
use super::*;
use serde_json::{Value, json};

fn anthropic() -> ProviderProfile {
    let mut profile = ProviderProfile::hosted(ProviderProtocol::Anthropic);
    profile.models = vec![agent_provider_config::ModelDescriptor::new("claude-x")];
    profile
}

fn sent(profile: &ProviderProfile, body: Value) -> Value {
    outbound_body(profile, "claude-x", body, false, false).unwrap()
}

fn markers(body: &Value) -> usize {
    cache_markers(body.as_object().unwrap()).len()
}

const EPHEMERAL: &str = "ephemeral";

/// The agent loop's turn: tools, a plain system prompt, the first user turn,
/// an assistant turn with thinking and tool calls, then its tool results.
fn loop_turn() -> Value {
    json!({
        "model": "claude-x",
        "max_tokens": 4096,
        "system": "You carry out a person's request.",
        "tools": [
            {"name": "list_operations", "input_schema": {"type": "object"}},
            {"name": "invoke", "input_schema": {
                "type": "object",
                // A schema property named like the marker is not a marker.
                "properties": {"cache_control": {"type": "string"}}
            }}
        ],
        "messages": [
            {"role": "user", "content": "Operations published right now: …"},
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "", "signature": "sig"},
                {"type": "tool_use", "id": "t1", "name": "invoke", "input": {"cache_control": "x"}},
                {"type": "tool_use", "id": "t2", "name": "invoke", "input": {}}
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "done"},
                {"type": "tool_result", "tool_use_id": "t2", "content": "done"}
            ]}
        ]
    })
}

#[test]
fn an_agent_turn_caches_system_tools_and_the_conversation_so_far() {
    let body = sent(&anthropic(), loop_turn());
    // The system string becomes one text block carrying the breakpoint.
    assert_eq!(
        body["system"],
        json!([{
            "type": "text",
            "text": "You carry out a person's request.",
            "cache_control": {"type": EPHEMERAL}
        }])
    );
    assert_eq!(body["tools"][1]["cache_control"]["type"], EPHEMERAL);
    assert!(body["tools"][0].get("cache_control").is_none());
    let messages = body["messages"].as_array().unwrap();
    // Only the last user message's last block; the first turn stays a string
    // and no assistant block (thinking or tool_use) is touched.
    assert_eq!(messages[0]["content"], "Operations published right now: …");
    assert!(
        messages[1]["content"]
            .as_array()
            .unwrap()
            .iter()
            .all(|block| block.get("cache_control").is_none())
    );
    assert!(messages[2]["content"][0].get("cache_control").is_none());
    assert_eq!(
        messages[2]["content"][1]["cache_control"]["type"],
        EPHEMERAL
    );
    assert_eq!(markers(&body), 3);
    // Everything else is the caller's request, byte for byte.
    assert_eq!(
        body["tools"][1]["input_schema"],
        loop_turn()["tools"][1]["input_schema"]
    );
    assert_eq!(messages[1], loop_turn()["messages"][1]);
}

#[test]
fn the_first_turn_marks_its_string_user_turn() {
    let body = sent(
        &anthropic(),
        json!({"system": "s", "messages": [{"role": "user", "content": "the request"}]}),
    );
    assert_eq!(
        body["messages"][0]["content"],
        json!([{"type": "text", "text": "the request", "cache_control": {"type": EPHEMERAL}}])
    );
    // No tools: two breakpoints, nothing invented.
    assert!(body.get("tools").is_none());
    assert_eq!(markers(&body), 2);
}

#[test]
fn array_system_and_trailing_blocks_mark_the_last_cacheable_block() {
    let body = sent(
        &anthropic(),
        json!({
            "system": [{"type": "text", "text": "one"}, {"type": "text", "text": "two"}],
            "tools": [
                {"name": "kept", "input_schema": {"type": "object"}},
                {"name": "deferred", "defer_loading": true, "input_schema": {"type": "object"}}
            ],
            "messages": [
                {"role": "user", "content": [
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AA=="}},
                    {"type": "text", "text": "  "}
                ]},
                {"role": "assistant", "content": [{"type": "text", "text": "a prefill"}]}
            ]
        }),
    );
    assert!(body["system"][0].get("cache_control").is_none());
    assert_eq!(body["system"][1]["cache_control"]["type"], EPHEMERAL);
    // A deferred tool is not in the prefix; the last sent tool is marked.
    assert_eq!(body["tools"][0]["cache_control"]["type"], EPHEMERAL);
    assert!(body["tools"][1].get("cache_control").is_none());
    // A blank text block cannot carry a marker; the image before it does.
    let user = &body["messages"][0]["content"];
    assert_eq!(user[0]["cache_control"]["type"], EPHEMERAL);
    assert!(user[1].get("cache_control").is_none());
    assert!(
        body["messages"][1]["content"][0]
            .get("cache_control")
            .is_none()
    );
}

#[test]
fn blank_or_absent_parts_get_no_breakpoint() {
    let body = sent(
        &anthropic(),
        json!({"system": " ", "messages": [{"role": "user", "content": ""}], "tools": []}),
    );
    assert_eq!(body["system"], " ");
    assert_eq!(body["messages"][0]["content"], "");
    assert_eq!(markers(&body), 0);
    // A bare request (as the protocol tests send) is left as it was.
    let bare = sent(&anthropic(), json!({"max_tokens": 9}));
    assert_eq!(bare, json!({"model": "claude-x", "max_tokens": 9}));
}

#[test]
fn caller_breakpoints_count_against_the_limit_and_are_never_duplicated() {
    // Two of the caller's own, one inside a tool_result: two are left, for
    // the system prompt and the conversation tail; the tools go without.
    let mut body = loop_turn();
    body["messages"][0]["content"] =
        json!([{"type": "text", "text": "first", "cache_control": {"type": EPHEMERAL}}]);
    body["messages"][2]["content"][0]["content"] =
        json!([{"type": "text", "text": "r", "cache_control": {"type": EPHEMERAL, "ttl": "5m"}}]);
    let sent_body = sent(&anthropic(), body);
    assert_eq!(markers(&sent_body), MAX_CACHE_BREAKPOINTS);
    assert_eq!(sent_body["system"][0]["cache_control"]["type"], EPHEMERAL);
    assert_eq!(
        sent_body["messages"][2]["content"][1]["cache_control"]["type"],
        EPHEMERAL
    );
    assert!(sent_body["tools"][1].get("cache_control").is_none());

    // A caller that already marked the tail keeps that one marker, and the
    // slot it did not need goes to the tools.
    let mut body = loop_turn();
    body["messages"][2]["content"][1]["cache_control"] = json!({"type": EPHEMERAL});
    let sent_body = sent(&anthropic(), body);
    assert_eq!(markers(&sent_body), 3);
    assert_eq!(sent_body["tools"][1]["cache_control"]["type"], EPHEMERAL);

    // Four of the caller's own: nothing is added.
    let mut body = loop_turn();
    for tool in body["tools"].as_array_mut().unwrap() {
        tool["cache_control"] = json!({"type": EPHEMERAL});
    }
    body["messages"][0]["content"] =
        json!([{"type": "text", "text": "first", "cache_control": {"type": EPHEMERAL}}]);
    body["messages"][2]["content"][0]["cache_control"] = json!({"type": EPHEMERAL});
    let sent_body = sent(&anthropic(), body.clone());
    assert_eq!(markers(&sent_body), MAX_CACHE_BREAKPOINTS);
    assert_eq!(sent_body["system"], body["system"]);
    assert!(
        sent_body["messages"][2]["content"][1]
            .get("cache_control")
            .is_none()
    );
}

#[test]
fn gateway_caches_the_neutral_catalogue_without_changing_catalogue_or_task_text() {
    let content = json!([
        {"type": "text", "text": "Operations published right now: …\n"},
        {"type": "text", "text": "Memory: keep this.\nThe request: post hello"}
    ]);
    for first_turn in [true, false] {
        let mut request = loop_turn();
        request["messages"][0]["content"] = content.clone();
        if first_turn {
            request["messages"].as_array_mut().unwrap().truncate(1);
        }
        let body = sent(&anthropic(), request.clone());
        assert_eq!(markers(&body), MAX_CACHE_BREAKPOINTS);
        assert_eq!(body["system"][0]["cache_control"]["type"], EPHEMERAL);
        assert_eq!(body["tools"][1]["cache_control"]["type"], EPHEMERAL);
        let user = &body["messages"][0]["content"];
        assert_eq!(user.as_array().unwrap().len(), 2);
        assert_eq!(user[0]["cache_control"]["type"], EPHEMERAL);
        for index in 0..2 {
            assert_eq!(user[index]["type"], content[index]["type"]);
            assert_eq!(user[index]["text"], content[index]["text"]);
        }
        if first_turn {
            assert_eq!(user[1]["cache_control"]["type"], EPHEMERAL);
        } else {
            assert!(user[1].get("cache_control").is_none());
            assert_eq!(
                body["messages"][2]["content"][1]["cache_control"]["type"],
                EPHEMERAL
            );
        }
        assert_eq!(sent(&anthropic(), body.clone()), body);

        // Automatic caching owns the tail, but still caches the catalogue.
        request["cache_control"] = json!({"type": EPHEMERAL});
        let automatic = sent(&anthropic(), request.clone());
        assert_eq!(markers(&automatic), MAX_CACHE_BREAKPOINTS);
        assert_eq!(
            automatic["messages"][0]["content"][0]["cache_control"]["type"],
            EPHEMERAL
        );
        assert_eq!(automatic["messages"][0]["content"][1], content[1]);
        if !first_turn {
            assert_eq!(automatic["messages"][2], request["messages"][2]);
        }

        request["cache_control"] = json!({"type": EPHEMERAL, "ttl": "1h"});
        assert_eq!(sent(&anthropic(), request.clone()), request);
        let mut profile = anthropic();
        profile.omit_params = vec!["cache_control".into()];
        let omitted = sent(&profile, request);
        assert_eq!(markers(&omitted), 0);
        assert_eq!(omitted["messages"][0]["content"], content);
    }
}

/// The agent's first user turn on Anthropic: the catalogue in a text block of
/// its own, closed by the agent's breakpoint so it is cached across tasks,
/// then the memory and the request.
fn catalogue_first_turn() -> Value {
    json!([
        {"type": "text", "text": "Operations published right now: …", "cache_control": {"type": EPHEMERAL}},
        {"type": "text", "text": "The request: post hello"}
    ])
}

#[test]
fn the_agents_catalogue_breakpoint_leaves_the_tail_and_the_tools_theirs() {
    // Turn one: the first user message is also the last. The catalogue keeps
    // its one marker as it came, and the tail's goes on the request after it:
    // system + catalogue + tail + tools is the whole budget.
    let mut body = loop_turn();
    body["messages"] = json!([{"role": "user", "content": catalogue_first_turn()}]);
    let first = sent(&anthropic(), body);
    assert_eq!(markers(&first), MAX_CACHE_BREAKPOINTS);
    let content = &first["messages"][0]["content"];
    assert_eq!(content[0], catalogue_first_turn()[0]);
    assert_eq!(content[1]["cache_control"]["type"], EPHEMERAL);
    assert_eq!(first["system"][0]["cache_control"]["type"], EPHEMERAL);
    assert_eq!(first["tools"][1]["cache_control"]["type"], EPHEMERAL);
    assert_eq!(sent(&anthropic(), first.clone()), first);

    // A later turn: the tail moves to the newest tool results, the catalogue
    // keeps its own, and the first turn's request is not marked.
    let mut body = loop_turn();
    body["messages"][0]["content"] = catalogue_first_turn();
    let later = sent(&anthropic(), body);
    assert_eq!(markers(&later), MAX_CACHE_BREAKPOINTS);
    assert_eq!(later["messages"][0]["content"], catalogue_first_turn());
    assert!(
        later["messages"][2]["content"][0]
            .get("cache_control")
            .is_none()
    );
    assert_eq!(
        later["messages"][2]["content"][1]["cache_control"]["type"],
        EPHEMERAL
    );
    assert_eq!(later["system"][0]["cache_control"]["type"], EPHEMERAL);
    assert_eq!(later["tools"][1]["cache_control"]["type"], EPHEMERAL);

    // A first turn that is only the marked catalogue: that marker already
    // ends the conversation, so nothing is stacked on it and the slot it
    // would have taken stays free.
    let catalogue_only = json!([catalogue_first_turn()[0]]);
    let mut body = loop_turn();
    body["messages"] = json!([{"role": "user", "content": catalogue_only}]);
    let alone = sent(&anthropic(), body);
    assert_eq!(alone["messages"][0]["content"], catalogue_only);
    assert_eq!(alone["system"][0]["cache_control"]["type"], EPHEMERAL);
    assert_eq!(alone["tools"][1]["cache_control"]["type"], EPHEMERAL);
    assert_eq!(markers(&alone), 3);
}

#[test]
fn automatic_caching_keeps_the_tail_and_a_longer_ttl_keeps_everything() {
    // Top-level `cache_control` places the tail breakpoint itself and takes
    // one slot; the static prefix is still marked.
    let mut body = loop_turn();
    body["cache_control"] = json!({"type": EPHEMERAL});
    let sent_body = sent(&anthropic(), body);
    assert_eq!(sent_body["cache_control"]["type"], EPHEMERAL);
    assert_eq!(sent_body["system"][0]["cache_control"]["type"], EPHEMERAL);
    assert_eq!(sent_body["tools"][1]["cache_control"]["type"], EPHEMERAL);
    assert!(
        sent_body["messages"][2]["content"][1]
            .get("cache_control")
            .is_none()
    );
    assert_eq!(markers(&sent_body), 3);

    // A one-hour entry may not follow a five-minute one: a caller using a
    // longer TTL owns the layout and the request goes out as it came.
    for one_hour in [
        json!({"cache_control": {"type": EPHEMERAL, "ttl": "1h"}}),
        json!({"tools": [{"name": "t", "input_schema": {}, "cache_control": {"type": EPHEMERAL, "ttl": "1h"}}]}),
    ] {
        let mut body = loop_turn();
        for (key, value) in one_hour.as_object().unwrap() {
            body[key] = value.clone();
        }
        let mut expected = body.clone();
        expected["model"] = json!("claude-x");
        assert_eq!(sent(&anthropic(), body), expected);
    }
}

#[test]
fn breakpoints_are_idempotent() {
    let once = sent(&anthropic(), loop_turn());
    assert_eq!(sent(&anthropic(), once.clone()), once);
}

#[test]
fn a_profile_that_omits_cache_control_gets_no_breakpoints() {
    let mut profile = anthropic();
    profile.omit_params = vec!["cache_control".into()];
    let mut body = loop_turn();
    body["cache_control"] = json!({"type": EPHEMERAL});
    let sent_body = sent(&profile, body);
    assert!(sent_body.get("cache_control").is_none());
    assert_eq!(sent_body["system"], loop_turn()["system"]);
    assert_eq!(markers(&sent_body), 0);
}

#[test]
fn openai_compatible_bodies_get_no_breakpoints() {
    let mut profile = ProviderProfile::hosted(ProviderProtocol::OpenAiCompatible);
    profile.models = vec![agent_provider_config::ModelDescriptor::new("claude-x")];
    let caller = json!({
        "messages": [
            {"role": "system", "content": "system"},
            {"role": "user", "content": "the request"}
        ],
        "tools": [{"type": "function", "function": {"name": "invoke", "parameters": {}}}]
    });
    let body = outbound_body(&profile, "claude-x", caller.clone(), true, false).unwrap();
    assert!(!body.to_string().contains("cache_control"));
    assert_eq!(body["messages"], caller["messages"]);
    assert_eq!(body["tools"], caller["tools"]);
}

#[test]
fn cache_reads_and_writes_are_logged_from_anthropic_usage() {
    let mut timing = PlanTiming::start();
    timing.observe_body(
        &json!({
            "content": [{"type": "tool_use", "id": "t", "name": "invoke", "input": {}}],
            "usage": {
                "input_tokens": 40, "output_tokens": 9,
                "cache_read_input_tokens": 9000, "cache_creation_input_tokens": 310
            }
        })
        .to_string(),
    );
    assert_eq!(timing.prompt_tokens, Some(40));
    assert_eq!(timing.cached_tokens, Some(9000));
    assert_eq!(timing.cache_write_tokens, Some(310));
    assert_eq!(timing.tool_calls, 1);
    // OpenAI-compatible usage reports reads only.
    timing.observe_usage(Some(&json!({
        "prompt_tokens": 12000, "prompt_tokens_details": {"cached_tokens": 11520}
    })));
    assert_eq!(timing.cached_tokens, Some(11520));
    assert_eq!(timing.cache_write_tokens, None);
}
