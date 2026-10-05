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
