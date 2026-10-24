use super::*;
use serde_json::json;

fn google_peer(listener: std::net::TcpListener, streaming: bool) -> std::thread::JoinHandle<()> {
    use std::io::{BufRead as _, Read as _, Write as _};
    std::thread::spawn(move || {
        let extra = json!({"google":{"thought_signature":"opaque-signed-thought"}});
        for turn in 0..2 {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(tilo_test_env::hang_bound()))
                .unwrap();
            let mut reader = std::io::BufReader::new(&mut socket);
            let mut headers = String::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
                headers.push_str(&line);
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let headers = headers.to_ascii_lowercase();
            assert!(headers.starts_with("post /chat/completions "));
            assert!(headers.contains("authorization: bearer minted-test-token\r\n"));
            assert!(headers.contains("x-goog-user-project: personal-billing-project\r\n"));
            assert_eq!(body["model"], "checked-model");
            if turn == 1 {
                let messages = body["messages"].as_array().unwrap();
                let call = &messages
                    .iter()
                    .find(|message| message["role"] == "assistant")
                    .unwrap()["tool_calls"][0];
                assert_eq!(call["id"], "call_1");
                assert_eq!(call["function"]["name"], "lookup");
                assert_eq!(call["function"]["arguments"], "{\"count\":2}");
                assert_eq!(call["extra_content"], extra);
                let result = messages
                    .iter()
                    .find(|message| message["role"] == "tool")
                    .unwrap();
                assert_eq!(result["tool_call_id"], "call_1");
                assert_eq!(result["content"], "found");
            }
            let call = json!({"id":"call_1","type":"function","function":{"name":"lookup","arguments":"{\"count\":2}"},"extra_content":extra});
            let (content_type, reply) = if streaming && turn == 0 {
                assert_eq!(body["stream"], true);
                let mut call = call;
                call["index"] = json!(0);
                let delta = json!({"id":"reply","object":"chat.completion.chunk","created":1,"model":"checked-model","choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[call]},"finish_reason":null}]});
                let finish = json!({"id":"reply","object":"chat.completion.chunk","created":1,"model":"checked-model","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]});
                (
                    "text/event-stream",
                    format!("data: {delta}\n\ndata: {finish}\n\ndata: [DONE]\n\n"),
                )
            } else {
                let reply = json!({"id":"reply","object":"chat.completion","created":1,"model":"checked-model","choices":[{"index":0,"finish_reason":"tool_calls","message":{"role":"assistant","tool_calls":[call]}}]});
                ("application/json", reply.to_string())
            };
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
        }
    })
}

#[tokio::test(flavor = "current_thread")]
async fn google_billing_and_gemini_signatures_survive_unary_and_streamed_tool_history() {
    for streaming in [false, true] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut profile =
            ProviderProfile::google_vertex("personal-billing-project", "global", "checked-model");
        profile.endpoint = format!("http://{}", listener.local_addr().unwrap());
        let peer = google_peer(listener, streaming);
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
                authenticated: true,
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
        peer.join().unwrap();
    }
}
