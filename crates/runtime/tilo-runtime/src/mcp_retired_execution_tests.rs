//! Requests that retained a session before DELETE cannot resume it afterward.
use super::*;

#[test]
fn retired_execution_retained_server_refuses_tools_after_delete() {
    let host = StubHost::default();
    let endpoint = endpoint(host.clone(), None);
    run(async {
        let id = open(&endpoint, "retained execution").await;
        let retained = endpoint.find(&id).unwrap();
        let live_reply = retained
            .handle(json!({
                "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": {"name": "tilo_list_apps", "arguments": {}}
            }))
            .await
            .unwrap();
        assert!(live_reply.get("error").is_none());
        let deleted = endpoint
            .dispatch(
                &Method::DELETE,
                "/mcp",
                &headers(&[(SESSION_HEADER, &id)]),
                b"",
            )
            .await;
        assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
        assert!(endpoint.find(&id).is_none());
        let invocation = tilo_actions::InvocationId::new().0.to_string();
        for (name, arguments) in [
            ("tilo_list_apps", json!({})),
            ("tilo_status", json!({"invocation_id": invocation})),
            ("tilo_cancel", json!({"invocation_id": invocation})),
            (
                "tilo_invoke",
                json!({
                    "peer_id": 71, "revision": "1", "resource": "test.note:1",
                    "action": tilo_resource::APPLY, "arguments": {}
                }),
            ),
        ] {
            let reply = retained
                .handle(json!({
                    "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                    "params": {"name": name, "arguments": arguments}
                }))
                .await
                .unwrap();
            assert_eq!(reply["error"]["code"], -32000, "{name}: {reply}");
            assert_eq!(reply["error"]["message"], "execution session ended");
            assert!(reply.get("result").is_none());
        }
        assert!(host.invoked.lock().unwrap().is_empty());
    });
}
