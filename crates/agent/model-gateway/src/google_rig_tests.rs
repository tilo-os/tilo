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
