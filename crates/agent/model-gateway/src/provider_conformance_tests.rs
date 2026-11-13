use super::*;
use serde_json::json;
use test_http::{Reply, Responder};

fn peer(answer: &serde_json::Value) -> (ProviderProfile, Responder) {
    let body = answer.to_string();
    let responder = Responder::start(vec![
        Reply::status(200)
            .header("Content-Type", "application/json")
            .body(body),
    ]);
    let profile = ProviderProfile {
        endpoint: format!("{}/v1", responder.url()),
        ..ProviderProfile::default()
    };
    (profile, responder)
}

async fn invoke(
    path: &str,
    request: neutral::ModelRequest,
    answer: serde_json::Value,
) -> Result<Option<neutral::ModelResponse>, String> {
    // Held until the call answers, which is when the provider is spoken to.
    let (profile, _served) = peer(&answer);
    let client = provider_client().unwrap();
    if path == "legacy" {
        let body = json!({
            "model":"checked-model", "messages":[{"role":"user", "content":"question"}],
            "tools":request.tools.iter().map(|tool| json!({"type":"function", "function": {
                "name":tool.name, "description":tool.description, "parameters":tool.input_schema
            }})).collect::<Vec<_>>(),
            "tool_choice": if request.tool_choice == neutral::ToolChoice::None { "none" } else { "auto" },
            "max_tokens":64
        });
        legacy(
            &client,
            &profile,
            None,
            body,
            tilo_test_env::hang_bound(),
            None,
            false,
        )
        .await
        .map(|_| None)
        .map_err(|failure| failure.why)
    } else {
        let request = neutral_request(request).unwrap();
        let result = if path == "host" {
            host_complete(
                &client,
                &profile,
                None,
                "checked-model",
                request,
                tilo_test_env::hang_bound(),
                crate::openai::Target::Standard,
            )
            .await
        } else {
            execute(
                GatewayHttp {
                    client,
                    endpoint: provider_url(&profile),
                    profile,
                    model: "checked-model".into(),
                    streaming: false,
                    api: None,
                    target: crate::openai::Target::Standard,
                    retry_refusals: false,
                    stream_end: Arc::default(),
                    budget: tilo_test_env::hang_bound(),
                    body: None,
                    failure: Arc::default(),
                    status: Arc::default(),
                    unary_body: Arc::default(),
                    profile_headers: Vec::new(),
                },
                None,
                request,
                None,
            )
            .await
        };
        result.map_err(|failure| failure.why).and_then(|response| {
            neutral_response(response)
                .map(Some)
                .map_err(|error| error.to_string())
        })
    }
}

fn answer(finish: serde_json::Value, tool: bool) -> serde_json::Value {
    let message = if tool {
        json!({"role":"assistant", "content":null, "tool_calls":[{
            "id":"call_1", "type":"function", "function":{"name":"lookup", "arguments":"{\"count\":2}"}
        }]})
    } else {
        json!({"role":"assistant", "content":"answer"})
    };
    let mut response = json!({"id":"reply", "object":"chat.completion", "created":1, "model":"checked-model",
        "choices":[{"index":0, "message":message}],
        "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}});
    response["choices"][0]["finish_reason"] = finish;
    response
}

fn request() -> neutral::ModelRequest {
    let mut request = neutral::Conversation::new("instructions", "question").request(
        vec![neutral::ToolDefinition {
            name: "lookup".into(),
            description: "Look up a resource".into(),
            input_schema: json!({"type":"object"}),
        }],
        64,
    );
    request.tool_choice = neutral::ToolChoice::Auto;
    request
}

#[tokio::test(flavor = "current_thread")]
async fn all_entry_points_reject_unsolicited_tools_and_accept_declared_names() {
    for path in ["neutral", "host", "legacy"] {
        for case in ["empty", "none", "unknown", "declared"] {
            let mut request = request();
            match case {
                "empty" => request.tools.clear(),
                "none" => request.tool_choice = neutral::ToolChoice::None,
                "unknown" => request.tools[0].name = "different".into(),
                _ => {}
            }
            let result = invoke(path, request, answer(json!("tool_calls"), true)).await;
            assert_eq!(
                result.is_ok(),
                case == "declared",
                "{path}/{case}: {result:?}"
            );
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn terminal_metadata_is_required_and_non_dispatchable_outcomes_survive() {
    for path in ["neutral", "host", "legacy"] {
        let mut missing = answer(serde_json::Value::Null, false);
        missing["choices"][0]
            .as_object_mut()
            .unwrap()
            .remove("finish_reason");
        assert!(invoke(path, request(), missing).await.is_err(), "{path}");
    }
    for path in ["neutral", "host"] {
        for (finish, reason) in [
            ("length", neutral::StopReason::OutputLimit),
            ("content_filter", neutral::StopReason::Refusal),
        ] {
            let response = invoke(path, request(), answer(json!(finish), true))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(response.stop_reason, reason, "{path}/{finish}");
            assert!(response.tool_calls().is_err());
        }
    }
}
