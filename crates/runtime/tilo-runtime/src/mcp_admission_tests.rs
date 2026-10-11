use super::*;

#[test]
fn admitted_runtime_factory_preserves_exact_caller_and_ignores_client_identity_labels() {
    let host = StubHost::default();
    let invoked = host.callers.clone();
    let actor = "04".repeat(32);
    let endpoint = Arc::new(Endpoint::new(
        host.clone(),
        "room".into(),
        actor,
        None,
        None,
    ));
    let created = Arc::new(Mutex::new(Vec::new()));
    let seen = created.clone();
    endpoint
        .serve_sessions(Arc::new(move || {
            let caller = Caller::authenticated([4; 32], [5; 32])
                .in_room("room")
                .in_new_execution();
            seen.lock().unwrap().push(caller.clone());
            McpServer::admitted(host.clone(), caller)
        }))
        .unwrap();
    run(async {
        let first = open(&endpoint, "forged actor execution:00000000").await;
        let second = open(&endpoint, "forged actor execution:00000000").await;
        invoke(&endpoint, &first).await;
        invoke(&endpoint, &second).await;
        invoke(&endpoint, &first).await;
        let created = created.lock().unwrap();
        let invoked = invoked.lock().unwrap();
        assert_eq!(created.len(), 2);
        assert_eq!(
            invoked.as_slice(),
            &[created[0].clone(), created[1].clone(), created[0].clone()]
        );
        drop(created);
        assert_eq!(invoked[0].user_public_key(), Some(&[4; 32]));
        assert_eq!(invoked[0].device_public_key(), Some(&[5; 32]));
        assert_ne!(invoked[0].execution_id(), invoked[1].execution_id());
        drop(invoked);
    });
}

#[test]
fn admitted_runtime_factory_failure_or_wrong_audience_never_falls_back_to_keyless_caller() {
    for wrong_audience in [false, true] {
        let host = StubHost::default();
        let called = host.callers.clone();
        let endpoint = Arc::new(Endpoint::new(
            host.clone(),
            "room".into(),
            "04".repeat(32),
            None,
            None,
        ));
        endpoint
            .serve_sessions(Arc::new(move || {
                if !wrong_audience {
                    return Err(std::io::Error::other("custody fenced"));
                }
                McpServer::admitted(
                    host.clone(),
                    Caller::authenticated([4; 32], [5; 32])
                        .in_room("other-room")
                        .in_new_execution(),
                )
            }))
            .unwrap();
        run(async {
            let response = endpoint
                .dispatch(
                    &Method::POST,
                    "/mcp",
                    &HeaderMap::new(),
                    &initialize("same label"),
                )
                .await;
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(endpoint.session_count(), 0);
            assert!(called.lock().unwrap().is_empty());
        });
    }
}

/// A desktop's agent door serves each connection as the agent the person
/// admitted, never as the person: every call reaches the host as that agent,
/// confined to the admitted room, in a fresh execution per connection, and
/// an agent session is offered no pixel tools.
#[test]
fn an_agent_session_acts_as_its_admitted_agent_in_its_own_execution() {
    let host = StubHost::default();
    let callers = host.callers.clone();
    let first = McpServer::for_agent(host.clone(), "room".into(), "claude-terminal");
    let second = McpServer::for_agent(host, "room".into(), "claude-terminal");
    let call = json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"tilo_invoke","arguments":{"peer_id":71,"revision":"1","resource":"test.note:1","action":tilo_resource::APPLY,"arguments":{}}}});
    run(async {
        for server in [&first, &second] {
            let initialized = server
                .handle(serde_json::from_slice(&initialize("Claude Code")).unwrap())
                .await
                .unwrap();
            assert!(initialized.get("result").is_some(), "{initialized}");
        }
        let tools = first
            .handle(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
            .await
            .unwrap();
        let names: Vec<_> = tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        assert!(names.contains(&"tilo_invoke"), "{names:?}");
        assert!(
            !names.iter().any(|name| name.starts_with("computer")),
            "{names:?}"
        );
        for server in [&first, &second] {
            let reply = server.handle(call.clone()).await.unwrap();
            assert_eq!(reply["result"]["isError"], false, "{reply}");
        }
    });
    let callers = callers.lock().unwrap();
    assert_eq!(callers.len(), 2);
    for caller in callers.iter() {
        assert_eq!(
            caller.principal(),
            &tilo_actions::remote::CallerPrincipal::Agent("claude-terminal".into())
        );
        assert_eq!(caller.room(), Some("room"));
        assert!(caller.execution_is_live());
    }
    assert_ne!(callers[0].execution_id(), callers[1].execution_id());
    drop(callers);
    assert!(first.audience_matches("room", "agent:claude-terminal"));
    drop(first);
}
